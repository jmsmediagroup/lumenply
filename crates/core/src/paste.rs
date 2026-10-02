//! Edit ▸ Copy / Cut / Paste for pixels: what gets copied (a layer's
//! pixels under the selection, soft edges included) and the paste itself
//! (a new layer above the active one, one undo step).

use crate::commands::insert_above;
use crate::{Command, EditResult};
use lumenply_doc::{Document, Layer, LayerId, Selection};
use lumenply_tiles::{Raster, Rect, TileStore};

/// The pixels Copy takes from `store`: under the selection (its coverage
/// scales them, so soft edges stay soft) or, without one, the layer's
/// painted area on the canvas. With the top-left canvas position; `None`
/// when nothing is there.
pub fn copy_region(store: &TileStore, sel: Option<&Selection>, canvas: Rect) -> Option<(Raster, i32, i32)> {
    let area = match sel {
        Some(s) => s.tight_bounds(canvas),
        None => crate::snap::painted_bounds(store)?.intersect(&canvas),
    };
    if area.is_empty() {
        return None;
    }
    let mut r = store.to_raster(area);
    if let Some(s) = sel {
        for y in 0..area.h {
            for x in 0..area.w {
                let k = s.value(area.x + x as i32, area.y + y as i32).clamp(0.0, 1.0);
                if k < 1.0 {
                    let i = (y * area.w + x) as usize;
                    r.pixels[i] = r.pixels[i].scale(k);
                }
            }
        }
    }
    r.pixels.iter().any(|p| p.a > 0.0).then_some((r, area.x, area.y))
}

/// Paste `raster` with its top-left at (`x`, `y`) as a new pixel layer
/// directly above `above` (or on top). Its id is `next_id()` before the
/// command runs.
pub struct PasteLayer {
    pub name: String,
    pub raster: Raster,
    pub x: i32,
    pub y: i32,
    pub above: Option<LayerId>,
}

impl Command for PasteLayer {
    fn label(&self) -> String {
        "Paste".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::new(self.x, self.y, self.raster.width, self.raster.height))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let id = doc.alloc_id();
        let mut layer = Layer::pixel(id, self.name.clone());
        *layer.pixels_mut().expect("a pixel layer") = TileStore::from_raster(&self.raster, self.x, self.y);
        insert_above(doc, layer, self.above)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_tiles::Rgba;

    fn red_layer_doc() -> (Document, LayerId, LayerId) {
        let mut doc = Document::new(40, 30);
        let bottom = doc.add_pixel_layer("bottom");
        let top = doc.add_pixel_layer("top");
        for y in 0..30 {
            for x in 0..40 {
                doc.layer_mut(bottom).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        (doc, bottom, top)
    }

    #[test]
    fn copy_takes_the_selected_pixels_with_soft_edges() {
        let (doc, bottom, _) = red_layer_doc();
        let store = doc.layer(bottom).unwrap().pixels().unwrap();
        let mut sel = Selection::rect(Rect::new(10, 5, 8, 6));
        let (r, x, y) = copy_region(store, Some(&sel), doc.canvas()).unwrap();
        assert_eq!((x, y, r.width, r.height), (10, 5, 8, 6));
        assert_eq!(r.get(0, 0), Rgba::new(1.0, 0.0, 0.0, 1.0));
        // A feathered selection hands over partial coverage.
        sel.feather(2.0);
        let (r, x, y) = copy_region(store, Some(&sel), doc.canvas()).unwrap();
        let partial = r.pixels.iter().filter(|p| p.a > 0.1 && p.a < 0.9).count();
        assert!(partial > 10, "soft edge pixels: {partial}");
        // The box's centre (14, 8) stays nearly solid.
        let mid = r.get((14 - x) as u32, (8 - y) as u32).a;
        assert!(mid > 0.9, "centre alpha {mid}");
        // Without a selection: the painted area, here the whole canvas.
        let (r, x, y) = copy_region(store, None, doc.canvas()).unwrap();
        assert_eq!((x, y, r.width, r.height), (0, 0, 40, 30));
        // An empty layer has nothing to copy.
        assert!(copy_region(&TileStore::new(), None, doc.canvas()).is_none());
    }

    #[test]
    fn paste_lands_above_the_given_layer_in_place() {
        let (doc, bottom, top) = red_layer_doc();
        let mut ed = Editor::new(doc);
        let new = ed.doc().next_id();
        let mut r = Raster::new(4, 3);
        for p in &mut r.pixels {
            *p = Rgba::new(0.0, 0.0, 1.0, 1.0);
        }
        ed.execute(&PasteLayer {
            name: "Pasted".into(),
            raster: r,
            x: 7,
            y: 9,
            above: Some(bottom),
        })
        .unwrap();
        let ids: Vec<_> = ed.doc().layers().iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![bottom, new, top]);
        let px = ed.doc().layer(new).unwrap().pixels().unwrap();
        assert_eq!(px.get_pixel(7, 9), Rgba::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(px.get_pixel(10, 11), Rgba::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(px.get_pixel(11, 9).a, 0.0);
        assert_eq!(ed.history().last(), Some(&"Paste"));
    }
}
