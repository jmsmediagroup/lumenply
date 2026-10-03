//! Everyday Photoshop edits: Layer via Cut, a layer's pixels as a
//! selection (Cmd-click its thumbnail), Edit ▸ Stroke, Select ▸ Reselect.

use std::sync::Arc;

use lumenply_doc::selection_ops::EdgeOp;
use lumenply_doc::{BlendMode, CombineOp, Document, LayerId, Mask, Selection};
use lumenply_tiles::{Rect, Rgba, Tile, TileStore};

use crate::commands::{NewLayerFromSelection, SetSelection};
use crate::{Command, EditError, EditResult};

/// Layer ▸ New ▸ Layer via Cut (Shift+Cmd+J): the selected pixels move
/// to a new layer directly above (soft edges split between the two); no
/// selection moves the whole layer's pixels.
pub struct LayerViaCut {
    pub layer: LayerId,
    pub name: String,
}

impl Command for LayerViaCut {
    fn label(&self) -> String {
        "Layer via cut".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        NewLayerFromSelection {
            layer: self.layer,
            name: self.name.clone(),
        }
        .apply(doc)?;
        let sel = doc.selection.clone();
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = l.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        match sel {
            None => *store = TileStore::new(),
            Some(sel) => {
                for c in store.coords().collect::<Vec<_>>() {
                    let Some(tile) = store.tile(c) else { continue };
                    let mut t = tile.clone();
                    let (ox, oy) = c.origin();
                    for (i, p) in t.pixels_mut().iter_mut().enumerate() {
                        let x = ox + (i % lumenply_tiles::TILE_SIZE) as i32;
                        let y = oy + (i / lumenply_tiles::TILE_SIZE) as i32;
                        let v = sel.value(x, y);
                        if v > 0.0 {
                            *p = p.scale(1.0 - v);
                        }
                    }
                    store.insert(c, Arc::new(t));
                }
                store.prune_blank();
            }
        }
        store.compact();
        Ok(())
    }
}

/// Cmd-click a layer thumbnail: the layer's opacity (its pixels' alpha)
/// as the selection, or combined with the current one (Shift adds, Alt
/// subtracts, both intersect).
pub struct SelectLayerPixels {
    pub layer: LayerId,
    pub op: CombineOp,
}

impl SelectLayerPixels {
    /// The layer's coverage as a selection.
    pub fn coverage(doc: &Document, layer: LayerId) -> Result<Selection, EditError> {
        let l = doc.layer(layer).ok_or(EditError::NoLayer(layer))?;
        let store = l
            .raster_store()
            .ok_or_else(|| EditError::Invalid("this layer has no pixels to select".into()))?;
        let mut tiles = TileStore::new();
        for c in store.coords() {
            let Some(tile) = store.tile(c) else { continue };
            let px = tile.pixels();
            let mut t = Tile::new();
            for (dst, p) in t.pixels_mut().iter_mut().zip(px.iter()) {
                let a = p.a.clamp(0.0, 1.0);
                *dst = Rgba::new(a, a, a, a);
            }
            tiles.insert(c, Arc::new(t));
        }
        tiles.prune_blank();
        Ok(Selection::from_mask(&Mask {
            tiles,
            default: 0.0,
            enabled: true,
        }))
    }
}

impl Command for SelectLayerPixels {
    fn label(&self) -> String {
        "Select layer pixels".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let mut sel = Self::coverage(doc, self.layer)?;
        if let (Some(cur), false) = (&doc.selection, self.op == CombineOp::Replace) {
            let mut combined = cur.clone();
            combined.combine(&sel, self.op);
            sel = combined;
        }
        SetSelection { selection: Some(sel) }.apply(doc)
    }
}

/// Where Edit ▸ Stroke draws relative to the selection's edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeLocation {
    Inside,
    Center,
    Outside,
}

/// Edit ▸ Stroke: paint a band of `color` (straight linear RGB) along the
/// selection's edge onto a pixel layer.
pub struct StrokeSelection {
    pub layer: LayerId,
    pub width: f32,
    pub color: [f32; 3],
    pub opacity: f32,
    pub location: StrokeLocation,
}

impl StrokeSelection {
    /// The band's coverage.
    pub fn band(&self, sel: &Selection, canvas: Rect) -> Selection {
        let w = self.width.clamp(1.0, 250.0);
        match self.location {
            StrokeLocation::Center => {
                let mut s = sel.clone();
                s.modify_edge(EdgeOp::Border(w), canvas);
                s
            }
            StrokeLocation::Outside => {
                let mut s = sel.clone();
                s.modify_edge(EdgeOp::Expand(w), canvas);
                s.combine(sel, CombineOp::Subtract);
                s
            }
            StrokeLocation::Inside => {
                let mut inner = sel.clone();
                inner.modify_edge(EdgeOp::Contract(w), canvas);
                let mut s = sel.clone();
                s.combine(&inner, CombineOp::Subtract);
                s
            }
        }
    }
}

impl Command for StrokeSelection {
    fn label(&self) -> String {
        "Stroke".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let sel = doc.selection.as_ref()?;
        let b = sel.bounds_within(doc.canvas());
        let pad = self.width.ceil() as i32 + 2;
        Some(
            Rect::new(b.x - pad, b.y - pad, b.w + 2 * pad as u32, b.h + 2 * pad as u32)
                .intersect(&doc.canvas()),
        )
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .selection
            .clone()
            .ok_or_else(|| EditError::Invalid("make a selection to stroke first".into()))?;
        let canvas = doc.canvas();
        let band = self.band(&sel, canvas);
        let region = band.bounds_within(canvas);
        let opacity = self.opacity.clamp(0.0, 1.0);
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = l.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        for c in region.tiles() {
            let (ox, oy) = c.origin();
            let mut t = store.tile(c).cloned().unwrap_or_else(Tile::new);
            let sub = region.intersect(&c.rect());
            let mut any = false;
            for y in sub.y..sub.bottom() {
                for x in sub.x..sub.right() {
                    let v = band.value(x, y) * opacity;
                    if v <= 0.0 {
                        continue;
                    }
                    let (tx, ty) = ((x - ox) as usize, (y - oy) as usize);
                    let paint = Rgba::from_straight(self.color[0], self.color[1], self.color[2], v);
                    let p = lumenply_render::blend_pixel(t.get(tx, ty), paint, BlendMode::Normal, 1.0);
                    t.set(tx, ty, p);
                    any = true;
                }
            }
            if any {
                store.insert(c, Arc::new(t));
            }
        }
        store.compact();
        Ok(())
    }
}

/// Select ▸ Reselect (Shift+Cmd+D): the selection most recently cleared.
pub struct Reselect;

impl Command for Reselect {
    fn label(&self) -> String {
        "Reselect".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .last_selection
            .clone()
            .ok_or_else(|| EditError::Invalid("nothing to reselect".into()))?;
        doc.selection = Some(sel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    fn red_square(doc: &mut Document) -> LayerId {
        let id = doc.add_pixel_layer("photo");
        for y in 10..30 {
            for x in 10..30 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        id
    }

    #[test]
    fn layer_via_cut_moves_the_selected_pixels_up() {
        let mut doc = Document::new(40, 40);
        let id = red_square(&mut doc);
        let mut ed = Editor::new(doc);
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 10, 10, 20))),
        })
        .unwrap();
        ed.execute(&LayerViaCut {
            layer: id,
            name: "Cut".into(),
        })
        .unwrap();
        let layers = ed.doc().layers();
        assert_eq!(layers.len(), 2);
        let (below, above) = (&layers[0], &layers[1]);
        assert_eq!(above.name, "Cut");
        let px = |l: &lumenply_doc::Layer, x, y| l.pixels().unwrap().get_pixel(x, y).a;
        assert_eq!((px(above, 15, 15), px(above, 25, 15)), (1.0, 0.0));
        assert_eq!((px(below, 15, 15), px(below, 25, 15)), (0.0, 1.0));
        // A pixel-locked layer refuses.
        let mut doc = Document::new(40, 40);
        let id = red_square(&mut doc);
        doc.layer_mut(id).unwrap().locks.pixels = true;
        let mut ed = Editor::new(doc);
        assert!(ed
            .execute(&LayerViaCut {
                layer: id,
                name: "Cut".into()
            })
            .is_err());
    }

    #[test]
    fn layer_pixels_become_the_selection_and_combine() {
        let mut doc = Document::new(40, 40);
        let id = red_square(&mut doc);
        let mut ed = Editor::new(doc);
        ed.execute(&SelectLayerPixels {
            layer: id,
            op: CombineOp::Replace,
        })
        .unwrap();
        let s = ed.doc().selection.clone().unwrap();
        assert_eq!(
            (s.value(10, 10), s.value(29, 29), s.value(9, 10), s.value(30, 30)),
            (1.0, 1.0, 0.0, 0.0)
        );
        // Subtract a rectangle that was selected before.
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 20, 40))),
        })
        .unwrap();
        ed.execute(&SelectLayerPixels {
            layer: id,
            op: CombineOp::Subtract,
        })
        .unwrap();
        let s = ed.doc().selection.clone().unwrap();
        assert_eq!((s.value(5, 5), s.value(15, 15)), (1.0, 0.0));
    }

    #[test]
    fn strokes_sit_inside_on_or_outside_the_edge() {
        let canvas = Rect::new(0, 0, 60, 60);
        let sel = Selection::rect(Rect::new(20, 20, 20, 20));
        let stroke = |location| StrokeSelection {
            layer: 0,
            width: 4.0,
            color: [0.0, 0.0, 1.0],
            opacity: 1.0,
            location,
        };
        let inside = stroke(StrokeLocation::Inside).band(&sel, canvas);
        assert_eq!(
            (inside.value(21, 30), inside.value(23, 30), inside.value(25, 30)),
            (1.0, 1.0, 0.0)
        );
        assert_eq!(inside.value(18, 30), 0.0);
        let outside = stroke(StrokeLocation::Outside).band(&sel, canvas);
        assert_eq!(
            (
                outside.value(18, 30),
                outside.value(16, 30),
                outside.value(21, 30)
            ),
            (1.0, 1.0, 0.0)
        );
        let center = stroke(StrokeLocation::Center).band(&sel, canvas);
        assert_eq!(
            (center.value(18, 30), center.value(21, 30), center.value(25, 30)),
            (1.0, 1.0, 0.0)
        );

        // Painted through the editor at 50%: blue over the red square's edge.
        let mut doc = Document::new(60, 60);
        let id = doc.add_pixel_layer("ink");
        doc.selection = Some(sel);
        let mut ed = Editor::new(doc);
        ed.execute(&StrokeSelection {
            layer: id,
            opacity: 0.5,
            ..stroke(StrokeLocation::Inside)
        })
        .unwrap();
        let p = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(21, 30);
        assert!(
            (p.a - 0.5).abs() < 1e-3 && (p.b - 0.5).abs() < 1e-3 && p.r == 0.0,
            "{p:?}"
        );
        assert_eq!(
            ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(30, 30).a,
            0.0
        );
    }

    #[test]
    fn reselect_brings_back_the_cleared_selection() {
        let mut ed = Editor::new(Document::new(20, 20));
        assert!(ed.execute(&Reselect).is_err(), "nothing cleared yet");
        let rect = Selection::rect(Rect::new(2, 3, 5, 6));
        ed.execute(&SetSelection {
            selection: Some(rect.clone()),
        })
        .unwrap();
        ed.execute(&SetSelection { selection: None }).unwrap();
        assert!(ed.doc().selection.is_none());
        ed.execute(&Reselect).unwrap();
        let s = ed.doc().selection.as_ref().unwrap();
        assert_eq!((s.value(2, 3), s.value(7, 3)), (1.0, 0.0));
    }
}
