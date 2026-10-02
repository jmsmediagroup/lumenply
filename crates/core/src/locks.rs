//! Layer locks (see `lumenply_doc::locks`): the command that sets them,
//! and the editor-side enforcement that every command passes through.
//!
//! Enforcement compares the command's target layer before and after the
//! command ran, so it covers every pixel edit (brushes, fills, gradients,
//! filters, clone and heal, text edits...) without each command having to
//! know about locks. Commands that move a layer say so through
//! [`Command::motion`].

use std::sync::Arc;

use lumenply_doc::{Document, Layer, LayerContent, LayerId, LayerLocks};
use lumenply_tiles::{Rect, Rgba, TileStore};

use crate::{Command, EditError, EditResult, Motion};

/// Set a layer's locks.
pub struct SetLayerLocks {
    pub layer: LayerId,
    pub locks: LayerLocks,
}

impl Command for SetLayerLocks {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.locks.is_empty() {
            "Unlock layer".into()
        } else {
            format!("Lock {}", self.locks.describe())
        }
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default()) // locks never change the picture
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.locks = self.locks;
        Ok(())
    }
}

/// The locks in force on `id`: its own plus every enclosing group's, with
/// "all" expanded (see [`LayerLocks::effective`]).
pub fn effective_locks(doc: &Document, id: LayerId) -> LayerLocks {
    let mut acc = doc.layer(id).map_or(LayerLocks::NONE, |l| l.locks);
    let mut cur = id;
    while let Some(p) = doc.parent_of(cur) {
        if let Some(g) = doc.layer(p) {
            acc = acc.union(&g.locks);
        }
        cur = p;
    }
    acc.effective()
}

fn locked(name: &str, why: &str) -> EditError {
    EditError::Invalid(format!("\"{name}\" is locked: {why}"))
}

/// Same tiles, by allocation (cheap: an edit replaces the tiles it touches).
fn same_store(a: &TileStore, b: &TileStore) -> bool {
    a.len() == b.len()
        && a.coords()
            .all(|c| matches!((a.tile_arc(c), b.tile_arc(c)), (Some(x), Some(y)) if Arc::ptr_eq(x, y)))
}

/// Did the layer's own picture data change (pixels, text, smart source)?
/// Text position is left out: moving text is a motion, not a pixel edit.
fn content_changed(old: &Layer, new: &Layer) -> bool {
    match (&old.content, &new.content) {
        (LayerContent::Pixel(a), LayerContent::Pixel(b)) => !same_store(a, b),
        (LayerContent::Text(a), LayerContent::Text(b)) => {
            let mut moved_back = b.clone();
            moved_back.x = a.x;
            moved_back.y = a.y;
            *a != moved_back
        }
        (LayerContent::Smart(a), LayerContent::Smart(b)) => !same_store(&a.source, &b.source),
        // A shape's placement is its transform; the rest is its picture.
        (LayerContent::Shape(a), LayerContent::Shape(b)) => {
            a.geometry != b.geometry || a.fill != b.fill || a.stroke != b.stroke
        }
        (LayerContent::Adjustment(a), LayerContent::Adjustment(b)) => a != b,
        (LayerContent::Filter(a), LayerContent::Filter(b)) => a != b,
        (LayerContent::Group(_), LayerContent::Group(_)) => false,
        _ => true, // rasterized, converted...
    }
}

/// Did anything but the layer's name, visibility, collapse state and locks
/// change (what "lock all" protects)?
fn properties_changed(old: &Layer, new: &Layer) -> bool {
    let mask_same = match (&old.mask, &new.mask) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.default == b.default && a.enabled == b.enabled && same_store(&a.tiles, &b.tiles)
        }
        _ => false,
    };
    old.opacity != new.opacity
        || old.blend != new.blend
        || old.effects != new.effects
        || old.clip != new.clip
        || old.pass_through != new.pass_through
        || !mask_same
}

/// Text and smart objects carry their placement in the layer data.
fn placement_changed(old: &Layer, new: &Layer) -> bool {
    match (&old.content, &new.content) {
        (LayerContent::Text(a), LayerContent::Text(b)) => a.x != b.x || a.y != b.y,
        (LayerContent::Smart(a), LayerContent::Smart(b)) => a.transform != b.transform,
        (LayerContent::Shape(a), LayerContent::Shape(b)) => a.transform != b.transform,
        _ => false,
    }
}

/// Keep a transparency-locked layer's alpha exactly as it was: every pixel
/// the command touched keeps its new colour at its old alpha. Pixels the
/// command erased keep their old value (erasing can't remove coverage).
fn restore_alpha(old: &TileStore, new: &mut TileStore) {
    let coords: Vec<_> = {
        let mut v: Vec<_> = new.coords().collect();
        v.extend(old.coords().filter(|c| new.tile(*c).is_none()));
        v
    };
    for c in coords {
        let (o, n) = (old.tile_arc(c), new.tile_arc(c));
        if let (Some(o), Some(n)) = (o, n) {
            if Arc::ptr_eq(o, n) {
                continue;
            }
        }
        let Some(o) = o.cloned() else {
            // Nothing was here: nothing may appear.
            new.remove(c);
            continue;
        };
        let Some(n) = n.cloned() else {
            new.insert(c, o); // the whole tile was cleared: put it back
            continue;
        };
        let op = o.pixels();
        let mut t = (*n).clone();
        for (i, p) in t.pixels_mut().iter_mut().enumerate() {
            let a = op[i].a;
            *p = if a <= 0.0 {
                Rgba::TRANSPARENT
            } else if p.a <= 1e-6 {
                op[i]
            } else {
                let k = a / p.a;
                Rgba::new(p.r * k, p.g * k, p.b * k, a)
            };
        }
        new.insert(c, Arc::new(t));
    }
    new.prune_blank();
}

/// Check `cmd`'s effect on its target layer against that layer's locks:
/// refuse it with a reason, or (transparency lock) put the alpha back.
/// Commands without a single target layer (structure, whole-image edits)
/// aren't limited: locked layers still reorder, group, resize with the
/// image and delete, as in Photoshop.
pub fn enforce(before: &Document, after: &mut Document, cmd: &dyn Command) -> EditResult {
    let Some(id) = cmd.target_layer() else {
        return Ok(());
    };
    let Some(old) = before.layer(id) else {
        return Ok(());
    };
    let locks = effective_locks(before, id);
    if locks.is_empty() {
        return Ok(());
    }
    let Some(new) = after.layer(id) else {
        return Ok(());
    };
    let name = old.name.as_str();
    if locks.all && properties_changed(old, new) {
        return Err(locked(name, "nothing about it can change until it is unlocked"));
    }
    let motion = match cmd.motion() {
        Motion::None if placement_changed(old, new) => Motion::Translate,
        m => m,
    };
    match motion {
        Motion::Translate if locks.position => return Err(locked(name, "its position is locked")),
        Motion::Translate => return Ok(()),
        Motion::Reshape if locks.position => return Err(locked(name, "its position is locked")),
        Motion::Reshape if locks.pixels => {
            return Err(locked(
                name,
                "its pixels are locked, so it can move but not reshape",
            ))
        }
        Motion::Reshape if locks.transparency => {
            return Err(locked(
                name,
                "its transparency is locked, so it can move but not reshape",
            ))
        }
        Motion::Reshape | Motion::None => {}
    }
    if !content_changed(old, new) {
        return Ok(());
    }
    if locks.pixels {
        return Err(locked(name, "its pixels are locked"));
    }
    if locks.transparency {
        let old_store = old.pixels().cloned();
        if let (Some(o), Some(store)) = (old_store, after.layer_mut(id).and_then(|l| l.pixels_mut())) {
            restore_alpha(&o, store);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::*;
    use crate::Editor;
    use lumenply_doc::BlendMode;
    use lumenply_tiles::{Affine, Raster};

    fn lock(transparency: bool, pixels: bool, position: bool, all: bool) -> LayerLocks {
        LayerLocks {
            transparency,
            pixels,
            position,
            all,
        }
    }

    /// One 64×64 layer holding a 20×20 square of 50 % blue at (10, 10).
    fn setup() -> (Editor, LayerId) {
        let mut ed = Editor::new(Document::new(64, 64));
        let id = ed.doc().next_id();
        let sq = Raster::filled(20, 20, Rgba::from_straight(0.0, 0.0, 1.0, 0.5));
        ed.execute(&AddPixelLayer::from_raster("Square", sq, 10, 10))
            .unwrap();
        (ed, id)
    }

    fn red_stroke(layer: LayerId) -> PaintStroke {
        PaintStroke {
            layer,
            brush: Brush {
                radius: 30.0,
                hardness: 1.0,
                color: [1.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: vec![StrokePoint::new(20.0, 20.0, 1.0)],
        }
    }

    #[test]
    fn transparency_lock_keeps_alpha_while_painting() {
        let (mut ed, id) = setup();
        ed.execute(&SetLayerLocks {
            layer: id,
            locks: lock(true, false, false, false),
        })
        .unwrap();
        ed.execute(&red_stroke(id)).unwrap();
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        // Inside the square: red at the square's 50 % alpha (premultiplied).
        let p = px.get_pixel(20, 20);
        assert!((p.r - 0.5).abs() < 1e-3 && p.g.abs() < 1e-3 && p.b.abs() < 1e-3 && (p.a - 0.5).abs() < 1e-3);
        // Outside it the stroke left nothing.
        assert_eq!(px.get_pixel(40, 20).a, 0.0);
        assert_eq!(px.get_pixel(5, 5).a, 0.0);
        // Erasing can't remove coverage either; fills obey too.
        let mut erase = red_stroke(id);
        erase.brush.mode = BrushMode::Erase;
        ed.execute(&erase).unwrap();
        assert!((ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(20, 20).a - 0.5).abs() < 1e-3);
        ed.execute(&Fill {
            layer: id,
            color: [0.0, 1.0, 0.0, 1.0],
        })
        .unwrap();
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let p = px.get_pixel(15, 15);
        assert!(p.r.abs() < 1e-3 && (p.g - 0.5).abs() < 1e-3 && (p.a - 0.5).abs() < 1e-3);
        assert_eq!(px.get_pixel(50, 50).a, 0.0);
        assert_eq!(px.content_bounds(), Some(Rect::new(10, 10, 20, 20)));
        // Moving is still allowed; reshaping is not.
        ed.execute(&MoveLayer {
            layer: id,
            dx: 5,
            dy: 0,
        })
        .unwrap();
        let err = ed
            .execute(&TransformLayer {
                layer: id,
                transform: Affine::scale(2.0, 2.0),
            })
            .unwrap_err();
        assert!(err.to_string().contains("can move but not reshape"), "{err}");
    }

    #[test]
    fn pixel_lock_refuses_edits_but_allows_moves() {
        let (mut ed, id) = setup();
        ed.execute(&SetLayerLocks {
            layer: id,
            locks: lock(false, true, false, false),
        })
        .unwrap();
        let steps = ed.history().len();
        let err = ed.execute(&red_stroke(id)).unwrap_err();
        assert_eq!(err.to_string(), "\"Square\" is locked: its pixels are locked");
        assert!(ed.execute(&Clear { layer: id }).is_err());
        assert!(ed
            .execute(&ApplyFilter {
                layer: id,
                filter: lumenply_doc::Filter::GaussianBlur { radius: 2.0 },
            })
            .is_err());
        assert!(ed
            .execute(&FlipLayer {
                layer: id,
                horizontal: true
            })
            .is_err());
        assert_eq!(ed.history().len(), steps, "refused edits leave no history");
        assert!((ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(20, 20).b - 0.5).abs() < 1e-3);
        // Moving, opacity and blend changes are fine.
        ed.execute(&MoveLayer {
            layer: id,
            dx: 3,
            dy: 4,
        })
        .unwrap();
        assert!((ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(13, 14).a - 0.5).abs() < 1e-3);
        ed.execute(&SetOpacity {
            layer: id,
            opacity: 0.3,
        })
        .unwrap();
        // Unlocking lets the stroke through.
        ed.execute(&SetLayerLocks {
            layer: id,
            locks: LayerLocks::NONE,
        })
        .unwrap();
        ed.execute(&red_stroke(id)).unwrap();
    }

    #[test]
    fn position_lock_refuses_moves_and_transforms_but_not_paint() {
        let (mut ed, id) = setup();
        ed.execute(&SetLayerLocks {
            layer: id,
            locks: lock(false, false, true, false),
        })
        .unwrap();
        let err = ed
            .execute(&MoveLayer {
                layer: id,
                dx: 1,
                dy: 0,
            })
            .unwrap_err();
        assert_eq!(err.to_string(), "\"Square\" is locked: its position is locked");
        assert!(ed
            .execute(&TransformLayer {
                layer: id,
                transform: Affine::scale(2.0, 1.0),
            })
            .is_err());
        ed.execute(&red_stroke(id)).unwrap();
        // Text keeps its place in the layer data: moving it is refused too.
        let t = ed.doc().next_id();
        ed.execute(&AddTextLayer {
            text: lumenply_doc::TextLayer::new("Hi", 5.0, 30.0, 12.0, [1.0; 4]),
            above: None,
        })
        .unwrap();
        ed.execute(&SetLayerLocks {
            layer: t,
            locks: lock(false, false, true, false),
        })
        .unwrap();
        let mut moved = ed.doc().layer(t).unwrap().text_layer().unwrap().clone();
        moved.x += 10.0;
        assert!(ed
            .execute(&SetText {
                layer: t,
                text: moved.clone()
            })
            .is_err());
        // Restyling in place is fine.
        let mut bold = ed.doc().layer(t).unwrap().text_layer().unwrap().clone();
        bold.bold = true;
        ed.execute(&SetText { layer: t, text: bold }).unwrap();
    }

    #[test]
    fn lock_all_freezes_properties_but_allows_rename_hide_and_delete() {
        let (mut ed, id) = setup();
        ed.execute(&SetLayerLocks {
            layer: id,
            locks: lock(false, false, false, true),
        })
        .unwrap();
        assert!(ed
            .execute(&SetOpacity {
                layer: id,
                opacity: 0.5
            })
            .is_err());
        assert!(ed
            .execute(&SetBlendMode {
                layer: id,
                blend: BlendMode::Multiply
            })
            .is_err());
        assert!(ed.execute(&AddMask { layer: id }).is_err());
        assert!(ed.execute(&red_stroke(id)).is_err());
        assert!(ed
            .execute(&MoveLayer {
                layer: id,
                dx: 1,
                dy: 1
            })
            .is_err());
        let err = ed
            .execute(&SetOpacity {
                layer: id,
                opacity: 0.5,
            })
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "\"Square\" is locked: nothing about it can change until it is unlocked"
        );
        ed.execute(&RenameLayer {
            layer: id,
            name: "Logo".into(),
        })
        .unwrap();
        ed.execute(&SetVisible {
            layer: id,
            visible: false,
        })
        .unwrap();
        ed.execute(&RemoveLayer { layer: id }).unwrap();
        assert!(ed.doc().layers().is_empty());
    }

    #[test]
    fn a_groups_locks_cover_its_children() {
        let (mut ed, id) = setup();
        let g = ed.doc().next_id();
        ed.execute(&GroupLayers {
            layers: vec![id],
            name: "G".into(),
        })
        .unwrap();
        ed.execute(&SetLayerLocks {
            layer: g,
            locks: lock(false, true, false, false),
        })
        .unwrap();
        assert_eq!(effective_locks(ed.doc(), id), lock(false, true, false, false));
        assert!(ed.execute(&red_stroke(id)).is_err());
        // Whole-image edits still apply to locked layers.
        ed.execute(&ResizeImage {
            width: 32,
            height: 32,
        })
        .unwrap();
        assert_eq!(ed.doc().width, 32);
    }
}
