//! Edit ▸ Puppet Warp: bakes a pinned as-rigid-as-possible mesh warp into a
//! pixel layer (see `lumenply_render::puppet`). The mesh is rebuilt from
//! the layer and the parameters, so the command is deterministic and the
//! workspace's preview and the bake agree.

use std::sync::Arc;

use crate::{Command, EditError, EditResult, Motion};
use lumenply_doc::{Document, LayerContent, LayerId};
use lumenply_render::puppet::{
    points_bounds, puppet_mask, puppet_store, MeshParams, Pt, PuppetMesh, PuppetMode, PuppetPin, PuppetSolver,
};
use lumenply_tiles::{Rect, TileStore};

/// Warp a pixel layer (and its masks) by pins on a mesh over its opaque
/// area. One undo step.
#[derive(Clone, Debug)]
pub struct PuppetWarp {
    pub layer: LayerId,
    pub mesh: MeshParams,
    pub mode: PuppetMode,
    pub pins: Vec<PuppetPin>,
}

/// A solved warp: the mesh, where its vertices go and the triangle order.
pub struct Solved {
    pub mesh: Arc<PuppetMesh>,
    pub pos: Vec<Pt>,
    pub order: Vec<u32>,
}

impl PuppetWarp {
    /// The warp of `store`, or `None` for an empty layer.
    pub fn solve(&self, store: &TileStore) -> Option<Solved> {
        let mesh = Arc::new(PuppetMesh::build(store, self.mesh)?);
        let from: Vec<Pt> = self.pins.iter().map(|p| p.from).collect();
        let to: Vec<Pt> = self.pins.iter().map(|p| p.to).collect();
        let pos = PuppetSolver::new(mesh.clone(), &from).solve(&to, self.mode);
        let order = mesh.draw_order(&self.pins);
        Some(Solved { mesh, pos, order })
    }

    /// True when no pin has moved (the warp changes nothing).
    pub fn is_identity(&self) -> bool {
        self.pins.iter().all(|p| p.from == p.to)
    }

    fn store<'a>(&self, doc: &'a Document) -> Option<&'a TileStore> {
        doc.layer(self.layer).and_then(|l| l.pixels())
    }
}

/// Where warped pixels may land: the canvas and a canvas-sized margin
/// around it, plus wherever the layer already is.
fn clip(doc: &Document, mesh: &PuppetMesh) -> Rect {
    let c = doc.canvas();
    Rect::new(c.x - c.w as i32, c.y - c.h as i32, c.w * 3, c.h * 3).union(&mesh.rest_bounds())
}

impl Command for PuppetWarp {
    fn label(&self) -> String {
        "Puppet Warp".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        Motion::Reshape
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let store = self.store(doc)?;
        let old = store.content_bounds()?;
        if self.is_identity() {
            return Some(old);
        }
        let new = self.solve(store).and_then(|s| points_bounds(&s.pos));
        Some(new.map_or(old, |n| old.union(&n)).intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        match &l.content {
            LayerContent::Pixel(_) => {}
            LayerContent::Text(_) => {
                return Err(EditError::Invalid(
                    "rasterize the text layer before puppet warping it".into(),
                ))
            }
            LayerContent::Smart(_) => {
                return Err(EditError::Invalid(
                    "smart objects keep affine transforms only; rasterize before puppet warping".into(),
                ))
            }
            _ => return Err(EditError::NotPixel(self.layer)),
        }
        let store = self.store(doc).ok_or(EditError::NotPixel(self.layer))?;
        let solved = self
            .solve(store)
            .ok_or_else(|| EditError::Invalid("the layer has no pixels to warp".into()))?;
        if self.is_identity() {
            return Ok(());
        }
        let clip = clip(doc, &solved.mesh);
        let Solved { mesh, pos, order } = solved;
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let LayerContent::Pixel(store) = &mut l.content else {
            return Err(EditError::NotPixel(self.layer));
        };
        *store = puppet_store(store, &mesh, &pos, &order, clip);
        if let Some(m) = l.mask.as_mut() {
            *m = puppet_mask(m, &mesh, &pos, &order, clip);
        }
        // The smart-filter mask follows the layer too (ADR 0011).
        if let Some(m) = l.smart_filters.mask.as_mut() {
            *m = puppet_mask(m, &mesh, &pos, &order, clip);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_doc::Mask;
    use lumenply_tiles::{Raster, Rgba};

    const RED: Rgba = Rgba::new(1.0, 0.0, 0.0, 1.0);

    /// A 100 × 80 document with a 20 × 10 red block at (20, 30).
    fn doc_with_block() -> (Document, LayerId) {
        let mut doc = Document::new(100, 80);
        let id = doc.add_pixel_layer("block");
        let store = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        *store = TileStore::from_raster(&Raster::filled(20, 10, RED), 20, 30);
        (doc, id)
    }

    /// Pins at two corners of the block, both moved by `(dx, dy)`.
    fn moved(id: LayerId, dx: f64, dy: f64) -> PuppetWarp {
        PuppetWarp {
            layer: id,
            mesh: MeshParams::default(),
            mode: PuppetMode::Normal,
            pins: [[22.0, 32.0], [38.0, 38.0]]
                .into_iter()
                .map(|p| PuppetPin {
                    from: p,
                    to: [p[0] + dx, p[1] + dy],
                    depth: 0,
                })
                .collect(),
        }
    }

    fn near(a: Rgba, b: Rgba) -> bool {
        (a.r - b.r).abs() < 1e-4
            && (a.g - b.g).abs() < 1e-4
            && (a.b - b.b).abs() < 1e-4
            && (a.a - b.a).abs() < 1e-4
    }

    #[test]
    fn puppet_warp_moves_the_layer_in_one_undo_step() {
        let (doc, id) = doc_with_block();
        let mut ed = Editor::new(doc);
        let cmd = moved(id, 15.0, 5.0);
        // Old content (20,30)-(40,40) ∪ the moved mesh: a 24 × 16 grid of
        // 4 px cells from (18, 28), moved to (33, 33).
        assert_eq!(cmd.affected(ed.doc()), Some(Rect::new(20, 30, 37, 19)));
        ed.execute(&cmd).unwrap();
        let px = |ed: &Editor, x, y| ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(x, y);
        assert!(near(px(&ed, 35, 35), RED), "{:?}", px(&ed, 35, 35));
        assert!(near(px(&ed, 54, 44), RED));
        assert_eq!(px(&ed, 34, 35).a, 0.0);
        assert_eq!(px(&ed, 25, 32).a, 0.0, "the old place is empty");
        assert_eq!(ed.history().last(), Some(&"Puppet Warp"));
        ed.undo();
        assert!(near(px(&ed, 25, 32), RED));
        assert_eq!(px(&ed, 54, 44).a, 0.0);
    }

    #[test]
    fn the_layer_mask_follows_the_warp() {
        let (mut doc, id) = doc_with_block();
        let mut m = Mask::reveal_all();
        m.fill_rect(Rect::new(25, 32, 4, 4), 0.0);
        doc.layer_mut(id).unwrap().mask = Some(m);
        let mut ed = Editor::new(doc);
        ed.execute(&moved(id, 15.0, 5.0)).unwrap();
        let m = ed.doc().layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(m.default, 1.0);
        assert!(m.value(41, 38) < 1e-4, "the hidden square moved");
        assert!((m.value(26, 33) - 1.0).abs() < 1e-4, "its old place is revealed");
    }

    #[test]
    fn unmoved_pins_change_nothing() {
        let (doc, id) = doc_with_block();
        let before = doc.layer(id).unwrap().pixels().unwrap().to_raster(doc.canvas());
        let mut ed = Editor::new(doc);
        let cmd = moved(id, 0.0, 0.0);
        assert!(cmd.is_identity());
        ed.execute(&cmd).unwrap();
        let after = ed
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .to_raster(ed.doc().canvas());
        assert_eq!(before, after);
    }

    #[test]
    fn puppet_warp_refuses_what_it_cannot_warp() {
        let mut doc = Document::new(40, 40);
        let empty = doc.add_pixel_layer("empty");
        let adj = doc.add_adjustment(lumenply_doc::Adjustment::Invert);
        let mut ed = Editor::new(doc);
        let run = |ed: &mut Editor, id| ed.execute(&moved(id, 3.0, 0.0));
        assert!(matches!(run(&mut ed, adj), Err(EditError::NotPixel(_))));
        match run(&mut ed, empty) {
            Err(EditError::Invalid(why)) => assert_eq!(why, "the layer has no pixels to warp"),
            other => panic!("{other:?}"),
        }
        // Smart objects refuse with a reason, like the other warps.
        let (doc, id) = doc_with_block();
        let mut ed = Editor::new(doc);
        ed.execute(&crate::commands::ConvertToSmartObject { layer: id })
            .unwrap();
        match ed.execute(&moved(id, 3.0, 0.0)) {
            Err(EditError::Invalid(why)) => assert!(why.contains("rasterize"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(ed.history().last(), Some(&"Convert to smart object"));
    }
}
