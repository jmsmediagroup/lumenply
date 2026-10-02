//! Smart filter commands (ADR 0011): add, edit, remove and reorder the
//! non-destructive filters on one layer, switch the whole stack, set its
//! filter mask. Each is one undo step; [`SetSmartFilter`] is meant for
//! `Editor::execute_coalescing` during slider drags. Re-exported from
//! [`crate::commands`].
//!
//! The filtered pixels are re-rendered by the editor after the command
//! (`lumenply_render::smart_filters::refresh_stale`), so these commands only
//! change the model.

use lumenply_doc::{Document, Filter, Layer, LayerContent, LayerId, Mask, SmartFilter, SmartFilters};
use lumenply_tiles::Rect;

use crate::{Command, EditError, EditResult};

/// Can `l` carry smart filters? Every layer with pixels of its own.
pub fn accepts_smart_filters(l: &Layer) -> bool {
    matches!(
        l.content,
        LayerContent::Pixel(_)
            | LayerContent::Smart(_)
            | LayerContent::Text(_)
            | LayerContent::Fill(_)
            | LayerContent::Shape(_)
    )
}

fn stack_mut(doc: &mut Document, id: LayerId) -> EditResult<&mut SmartFilters> {
    let l = doc.layer_mut(id).ok_or(EditError::NoLayer(id))?;
    if !accepts_smart_filters(l) {
        return Err(EditError::Invalid(format!(
            "layer '{}' cannot have smart filters",
            l.name
        )));
    }
    Ok(&mut l.smart_filters)
}

fn check_index(sf: &SmartFilters, index: usize) -> EditResult {
    if index < sf.filters.len() {
        Ok(())
    } else {
        Err(EditError::Invalid(format!("no smart filter at position {index}")))
    }
}

/// The canvas area a layer's filtered look can cover with a stack reaching
/// `pad` px: its pixels' bounds grown by the pad and by its effects.
fn reach(doc: &Document, id: LayerId, extra_pad: i32) -> Option<Rect> {
    let l = doc.layer(id)?;
    let b = l.content_store()?.bounds()?;
    let fx = if l.effects.is_empty() { 0 } else { l.effects.pad() };
    let p = l.smart_filters.pad().max(extra_pad) + fx;
    Some(Rect::new(b.x - p, b.y - p, b.w + 2 * p as u32, b.h + 2 * p as u32).intersect(&doc.canvas()))
}

/// Grow an edit's affected area by the document's largest smart-filter
/// reach: a pixel edit under a smart filter changes the filtered look that
/// far around it. The editor applies this to every command.
pub fn widen_affected(doc: &Document, r: Option<Rect>) -> Option<Rect> {
    let p = lumenply_render::smart_filters::max_pad(doc);
    match r {
        Some(r) if p > 0 && !r.is_empty() => {
            Some(Rect::new(r.x - p, r.y - p, r.w + 2 * p as u32, r.h + 2 * p as u32).intersect(&doc.canvas()))
        }
        other => other,
    }
}

/// Add a smart filter on top of a layer's stack (it runs last), or at
/// `index` in application order.
pub struct AddSmartFilter {
    pub layer: LayerId,
    pub filter: Filter,
    pub index: Option<usize>,
}

impl AddSmartFilter {
    pub fn new(layer: LayerId, filter: Filter) -> Self {
        AddSmartFilter {
            layer,
            filter,
            index: None,
        }
    }
}

impl Command for AddSmartFilter {
    fn label(&self) -> String {
        format!("Smart filter: {}", self.filter.name())
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        reach(
            doc,
            self.layer,
            doc.layer(self.layer)?.smart_filters.pad() + self.filter.pad(),
        )
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sf = stack_mut(doc, self.layer)?;
        let at = self.index.unwrap_or(sf.filters.len()).min(sf.filters.len());
        sf.filters.insert(at, SmartFilter::new(self.filter.clone()));
        // Adding a filter shows the stack again, as Photoshop does.
        sf.enabled = true;
        Ok(())
    }
}

/// Replace one smart filter: its parameters, opacity, blend mode and
/// enabled flag. Use `Editor::execute_coalescing` for slider drags.
pub struct SetSmartFilter {
    pub layer: LayerId,
    pub index: usize,
    pub filter: SmartFilter,
}

impl Command for SetSmartFilter {
    fn label(&self) -> String {
        format!("Edit smart filter {}", self.filter.filter.name())
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        reach(doc, self.layer, self.filter.filter.pad() * 2)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sf = stack_mut(doc, self.layer)?;
        check_index(sf, self.index)?;
        let mut f = self.filter.clone();
        f.opacity = if f.opacity.is_finite() {
            f.opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        sf.filters[self.index] = f;
        Ok(())
    }
}

/// Remove one smart filter from a layer.
pub struct RemoveSmartFilter {
    pub layer: LayerId,
    pub index: usize,
}

impl Command for RemoveSmartFilter {
    fn label(&self) -> String {
        "Delete smart filter".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        reach(doc, self.layer, 0)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sf = stack_mut(doc, self.layer)?;
        check_index(sf, self.index)?;
        sf.filters.remove(self.index);
        if sf.filters.is_empty() {
            // The mask belongs to the stack; Photoshop drops it with the
            // last filter.
            sf.mask = None;
        }
        Ok(())
    }
}

/// Move a smart filter from `from` to `to` (application order).
pub struct ReorderSmartFilter {
    pub layer: LayerId,
    pub from: usize,
    pub to: usize,
}

impl Command for ReorderSmartFilter {
    fn label(&self) -> String {
        "Reorder smart filters".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        reach(doc, self.layer, 0)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sf = stack_mut(doc, self.layer)?;
        check_index(sf, self.from)?;
        check_index(sf, self.to)?;
        if self.from == self.to {
            return Err(EditError::Invalid("the smart filter is already there".into()));
        }
        let f = sf.filters.remove(self.from);
        sf.filters.insert(self.to, f);
        Ok(())
    }
}

/// The eye on a layer's "Smart Filters" row: show or hide the whole stack.
pub struct SetSmartFiltersEnabled {
    pub layer: LayerId,
    pub enabled: bool,
}

impl Command for SetSmartFiltersEnabled {
    fn label(&self) -> String {
        if self.enabled {
            "Show smart filters".into()
        } else {
            "Hide smart filters".into()
        }
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let l = doc.layer(self.layer)?;
        let all: i32 = l.smart_filters.filters.iter().map(|f| f.filter.pad()).sum();
        reach(doc, self.layer, all)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        stack_mut(doc, self.layer)?.enabled = self.enabled;
        Ok(())
    }
}

/// Set or clear a layer's smart-filter mask (`None` removes it). With
/// `from_selection`, the document's selection becomes the mask instead
/// (revealing everything when there is none).
pub struct SetSmartFilterMask {
    pub layer: LayerId,
    pub mask: Option<Mask>,
    pub from_selection: bool,
}

impl Command for SetSmartFilterMask {
    fn label(&self) -> String {
        if self.mask.is_none() && !self.from_selection {
            "Delete filter mask".into()
        } else {
            "Filter mask".into()
        }
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        reach(doc, self.layer, 0)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let mask = if self.from_selection {
            Some(
                doc.selection
                    .as_ref()
                    .map_or_else(Mask::reveal_all, |s| s.to_mask()),
            )
        } else {
            self.mask.clone()
        };
        let sf = stack_mut(doc, self.layer)?;
        if sf.filters.is_empty() && mask.is_some() {
            return Err(EditError::Invalid("the layer has no smart filters".into()));
        }
        sf.mask = mask;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddPixelLayer, ConvertToSmartObject, RasterizeLayer, SetSelection};
    use crate::Editor;
    use lumenply_doc::{BlendMode, Selection};
    use lumenply_tiles::{Raster, Rgba};

    /// A 128×64 document: one pixel layer, white left of x = 64, black
    /// from there on.
    fn step_doc() -> (Editor, LayerId) {
        let mut r = Raster::new(128, 64);
        for y in 0..64 {
            for x in 0..128 {
                let v = if x < 64 { 1.0 } else { 0.0 };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let mut ed = Editor::new(Document::new(128, 64));
        ed.execute(&AddPixelLayer::from_raster("Step", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        (ed, id)
    }

    fn shown(ed: &Editor, id: LayerId, x: i32) -> f32 {
        ed.doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(x, 32)
            .r
    }

    fn blur() -> Filter {
        Filter::BoxBlur { radius: 2.0 }
    }

    #[test]
    fn add_smart_filter_filters_the_layer_and_undoes() {
        let (mut ed, id) = step_doc();
        ed.execute(&AddSmartFilter::new(id, blur())).unwrap();
        assert_eq!(ed.history().last().copied(), Some("Smart filter: Box Blur"));
        // Two white pixels in the 5-px window (16-bit storage: ±1e-4).
        assert!((shown(&ed, id, 64) - 0.4).abs() < 1e-4);
        assert!((shown(&ed, id, 65) - 0.2).abs() < 1e-4);
        // The layer's own pixels are untouched.
        let own = ed
            .doc()
            .layer(id)
            .unwrap()
            .content_store()
            .unwrap()
            .get_pixel(64, 32);
        assert_eq!(own.r, 0.0);
        // The affected area is the layer's bounds grown by the filter's
        // reach, within the canvas.
        assert_eq!(ed.last_affected(), Some(Rect::new(0, 0, 128, 64)));
        ed.undo();
        assert_eq!(shown(&ed, id, 64), 0.0);
        assert!(ed.doc().layer(id).unwrap().smart_filters.cache.is_none());
        ed.redo();
        assert!((shown(&ed, id, 64) - 0.4).abs() < 1e-4);
    }

    #[test]
    fn set_smart_filter_edits_params_opacity_and_blend_and_coalesces() {
        let (mut ed, id) = step_doc();
        ed.execute(&AddSmartFilter::new(id, blur())).unwrap();
        let mut f = SmartFilter::new(Filter::BoxBlur { radius: 1.0 });
        ed.execute(&SetSmartFilter {
            layer: id,
            index: 0,
            filter: f.clone(),
        })
        .unwrap();
        // A 3-px window: x = 64 sees one white pixel of three.
        assert!((shown(&ed, id, 64) - 1.0 / 3.0).abs() < 1e-4);
        f.opacity = 0.5;
        ed.execute(&SetSmartFilter {
            layer: id,
            index: 0,
            filter: f.clone(),
        })
        .unwrap();
        assert!((shown(&ed, id, 64) - 1.0 / 6.0).abs() < 1e-4);
        f.opacity = 1.0;
        f.blend = BlendMode::Darken;
        ed.execute(&SetSmartFilter {
            layer: id,
            index: 0,
            filter: f.clone(),
        })
        .unwrap();
        assert!(shown(&ed, id, 64).abs() < 1e-4);
        assert!((shown(&ed, id, 63) - 2.0 / 3.0).abs() < 1e-4);
        f.blend = BlendMode::Normal;
        f.enabled = false;
        ed.execute(&SetSmartFilter {
            layer: id,
            index: 0,
            filter: f.clone(),
        })
        .unwrap();
        assert_eq!(shown(&ed, id, 64), 0.0, "disabled shows the plain pixels");

        // A slider drag: many ticks, one undo step.
        f.enabled = true;
        let steps = ed.history().len();
        for r in 1..=4 {
            f.filter = Filter::BoxBlur { radius: r as f32 };
            ed.execute_coalescing(
                &SetSmartFilter {
                    layer: id,
                    index: 0,
                    filter: f.clone(),
                },
                "sf-radius",
            )
            .unwrap();
        }
        ed.end_coalescing();
        assert_eq!(ed.history().len(), steps + 1);
        // A 9-px window: x = 64 sees four white pixels of nine.
        assert!((shown(&ed, id, 64) - 4.0 / 9.0).abs() < 1e-4);
        ed.undo();
        assert_eq!(shown(&ed, id, 64), 0.0);
        assert!(ed
            .execute(&SetSmartFilter {
                layer: id,
                index: 3,
                filter: f,
            })
            .is_err());
    }

    #[test]
    fn remove_and_reorder_are_single_undo_steps() {
        let (mut ed, id) = step_doc();
        ed.execute(&AddSmartFilter::new(id, blur())).unwrap();
        ed.execute(&AddSmartFilter::new(id, Filter::Mosaic { size: 4.0 }))
            .unwrap();
        // Blur, then mosaic: the cell 64..68 averages 0.4, 0.2, 0, 0.
        assert!((shown(&ed, id, 64) - 0.15).abs() < 1e-4);
        ed.execute(&ReorderSmartFilter {
            layer: id,
            from: 1,
            to: 0,
        })
        .unwrap();
        let names: Vec<&str> = ed
            .doc()
            .layer(id)
            .unwrap()
            .smart_filters
            .filters
            .iter()
            .map(|f| f.filter.name())
            .collect();
        assert_eq!(names, ["Mosaic", "Box Blur"]);
        // Mosaic first keeps the edge; the blur ramp follows.
        assert!((shown(&ed, id, 64) - 0.4).abs() < 1e-4);
        ed.undo();
        assert!((shown(&ed, id, 64) - 0.15).abs() < 1e-4);
        ed.redo();

        ed.execute(&RemoveSmartFilter { layer: id, index: 1 }).unwrap();
        // Only the mosaic is left, and its cells match the step.
        assert_eq!(shown(&ed, id, 64), 0.0);
        assert_eq!(ed.doc().layer(id).unwrap().smart_filters.filters.len(), 1);
        ed.undo();
        assert!((shown(&ed, id, 64) - 0.4).abs() < 1e-4);
        assert!(ed.execute(&RemoveSmartFilter { layer: id, index: 5 }).is_err());
        assert!(ed
            .execute(&ReorderSmartFilter {
                layer: id,
                from: 0,
                to: 0
            })
            .is_err());
    }

    #[test]
    fn master_switch_mask_and_rasterize() {
        let (mut ed, id) = step_doc();
        ed.execute(&ConvertToSmartObject { layer: id }).unwrap();
        ed.execute(&AddSmartFilter::new(id, blur())).unwrap();
        assert!(
            (shown(&ed, id, 64) - 0.4).abs() < 1e-4,
            "smart objects take smart filters"
        );
        ed.execute(&SetSmartFiltersEnabled {
            layer: id,
            enabled: false,
        })
        .unwrap();
        assert_eq!(shown(&ed, id, 64), 0.0);
        ed.undo();
        assert!((shown(&ed, id, 64) - 0.4).abs() < 1e-4);

        // A filter mask from a selection: filtered only in the bottom half.
        let sel = Selection::rect(Rect::new(0, 32, 128, 32));
        ed.execute(&SetSelection { selection: Some(sel) }).unwrap();
        ed.execute(&SetSmartFilterMask {
            layer: id,
            mask: None,
            from_selection: true,
        })
        .unwrap();
        let px = |ed: &Editor, y: i32| {
            ed.doc()
                .layer(id)
                .unwrap()
                .raster_store()
                .unwrap()
                .get_pixel(64, y)
                .r
        };
        assert_eq!(px(&ed, 10), 0.0);
        assert!((px(&ed, 50) - 0.4).abs() < 1e-4);

        // Rasterizing bakes the filters (and the mask) into the pixels.
        ed.execute(&RasterizeLayer { layer: id }).unwrap();
        let l = ed.doc().layer(id).unwrap();
        assert!(l.smart_filters.is_empty() && l.smart_filters.mask.is_none());
        let p = l.pixels().unwrap();
        assert_eq!(p.get_pixel(64, 10).r, 0.0);
        assert!((p.get_pixel(64, 50).r - 0.4).abs() < 1e-4);
    }

    #[test]
    fn painting_under_a_smart_filter_widens_the_affected_area() {
        let (mut ed, id) = step_doc();
        ed.execute(&AddSmartFilter::new(id, Filter::BoxBlur { radius: 5.0 }))
            .unwrap();
        ed.execute(&crate::commands::PaintStroke {
            layer: id,
            brush: crate::commands::Brush {
                radius: 2.0,
                hardness: 1.0,
                color: [1.0, 0.0, 0.0, 1.0],
                ..crate::commands::Brush::default()
            },
            points: vec![crate::commands::StrokePoint::new(100.0, 30.0, 1.0)],
        })
        .unwrap();
        let r = ed.last_affected().unwrap();
        // The dab reaches about 3 px; the blur 5 px further.
        assert!(r.contains(92, 30) && r.contains(108, 30), "{r:?}");
        assert!(!r.contains(80, 30), "{r:?}");
        // Paint shows through the blur: red spreads to x = 105.
        let p = ed
            .doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(104, 30);
        assert!(p.r > 0.0 && p.g < p.r, "{p:?}");
    }

    #[test]
    fn layers_without_pixels_refuse_smart_filters() {
        let mut ed = Editor::new(Document::new(16, 16));
        ed.execute(&crate::commands::AddAdjustmentLayer::new(
            lumenply_doc::Adjustment::Invert,
        ))
        .unwrap();
        let id = ed.doc().layers()[0].id;
        assert!(ed.execute(&AddSmartFilter::new(id, blur())).is_err());
        assert!(ed.execute(&AddSmartFilter::new(999, blur())).is_err());
    }

    #[test]
    fn the_filter_mask_moves_with_the_layer() {
        let (mut ed, id) = step_doc();
        ed.execute(&AddSmartFilter::new(id, blur())).unwrap();
        // Mask the filters out of the top half, then move the layer down
        // 8 px: the mask's edge follows from y = 32 to y = 40.
        ed.execute(&SetSelection {
            selection: Some(lumenply_doc::Selection::rect(Rect::new(0, 32, 128, 32))),
        })
        .unwrap();
        ed.execute(&SetSmartFilterMask {
            layer: id,
            mask: None,
            from_selection: true,
        })
        .unwrap();
        ed.execute(&crate::commands::MoveLayer {
            layer: id,
            dx: 0,
            dy: 8,
        })
        .unwrap();
        let m = ed.doc().layer(id).unwrap().smart_filters.mask.as_ref().unwrap();
        assert_eq!((m.value(10, 39), m.value(10, 40)), (0.0, 1.0));
        let px = |y: i32| {
            ed.doc()
                .layer(id)
                .unwrap()
                .raster_store()
                .unwrap()
                .get_pixel(64, y)
                .r
        };
        assert_eq!(px(36), 0.0, "still masked after the move");
        assert!((px(50) - 0.4).abs() < 1e-4);
    }
}
