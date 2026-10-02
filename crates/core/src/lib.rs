//! The app core: every edit is a [`Command`] applied through an [`Editor`],
//! which keeps undo/redo history as document snapshots.
//!
//! Snapshots are cheap because tiles are shared copy-on-write: an undo step
//! costs only the tiles the command actually touched. Tools, scripts and
//! plugins all go through this one path, so undo, macros and the headless
//! CLI behave identically.

pub mod align;
pub mod commands;
pub mod demo;
pub mod layer_ops;
pub mod locks;

use lumenply_doc::{Document, LayerId};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("no layer with id {0}")]
    NoLayer(LayerId),
    #[error("layer {0} is not a pixel layer")]
    NotPixel(LayerId),
    #[error("layer {0} is not a group")]
    NotGroup(LayerId),
    #[error("{0}")]
    Invalid(String),
}

pub type EditResult<T = ()> = Result<T, EditError>;

/// How a command moves its target layer, so layer locks can judge it
/// (see [`locks::enforce`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Doesn't move the layer.
    #[default]
    None,
    /// Shifts it by whole pixels: only a position lock refuses this.
    Translate,
    /// Scales, rotates, flips, distorts or warps it: position, pixel and
    /// transparency locks all refuse this.
    Reshape,
}

/// An undoable edit. Commands must be deterministic: the editor records only
/// the resulting document, not the command itself.
pub trait Command {
    /// Short label for the history panel, e.g. "Paint stroke".
    fn label(&self) -> String;
    fn apply(&self, doc: &mut Document) -> EditResult;
    /// Canvas area whose appearance may change, computed against the
    /// document *before* the command runs. `None` means "anything".
    /// Front ends use it to recomposite only what moved.
    fn affected(&self, _doc: &Document) -> Option<lumenply_tiles::Rect> {
        None
    }
    /// The single layer whose data or properties this command changes, if it
    /// can name one. `None` means the change may touch anything (structure,
    /// several layers, the canvas). Front ends use it to keep composite
    /// caches warm across consecutive edits to the same layer.
    fn target_layer(&self) -> Option<LayerId> {
        None
    }
    /// How this command moves [`Command::target_layer`]; layer locks use
    /// it to tell a move from a pixel edit.
    fn motion(&self) -> Motion {
        Motion::None
    }
}

struct Snapshot {
    label: String,
    doc: Document,
    /// Canvas area the step changed (see [`Command::affected`]); undoing or
    /// redoing the step dirties exactly this area. `None` means "anything".
    affected: Option<lumenply_tiles::Rect>,
    /// Estimated bytes this snapshot keeps alive: its tiles that the state
    /// replacing it no longer shares.
    bytes: usize,
}

/// Estimated bytes of tile data in `old` that `new` does not share (compared
/// per layer id and tile coordinate by allocation identity). This is what
/// dropping `old` from the history would free, modulo sharing with even
/// older snapshots.
fn delta_bytes(old: &Document, new: &Document) -> usize {
    fn store_delta(old: &lumenply_tiles::TileStore, new: Option<&lumenply_tiles::TileStore>) -> usize {
        old.coords()
            .filter_map(|c| {
                let ot = old.tile(c)?;
                let shared = new.and_then(|n| n.tile(c)).is_some_and(|nt| std::ptr::eq(ot, nt));
                (!shared).then(|| ot.byte_size())
            })
            .sum()
    }
    let mut total = 0;
    old.for_each_layer(|l| {
        let counterpart = new.layer(l.id);
        if let Some(s) = l.raster_store() {
            total += store_delta(s, counterpart.and_then(|n| n.raster_store()));
        }
        if let Some(m) = &l.mask {
            total += store_delta(
                &m.tiles,
                counterpart.and_then(|n| n.mask.as_ref()).map(|nm| &nm.tiles),
            );
        }
    });
    total
}

fn union_opt(
    a: Option<lumenply_tiles::Rect>,
    b: Option<lumenply_tiles::Rect>,
) -> Option<lumenply_tiles::Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(&b)),
        _ => None,
    }
}

/// Put every tile the document owns outright into compact 16-bit storage.
/// Tiles shared with history snapshots are left as they are, so this costs
/// only the tiles a command actually changed. Float-mode documents skip
/// compaction entirely, so HDR values survive every edit (ADR 0004).
pub fn compact_storage(doc: &mut Document) {
    if doc.float_mode {
        return;
    }
    doc.for_each_layer_mut(|l| {
        match &mut l.content {
            lumenply_doc::LayerContent::Pixel(store) => store.compact(),
            lumenply_doc::LayerContent::Text(t) => {
                if let Some(c) = t.cache.as_mut() {
                    c.compact();
                }
            }
            lumenply_doc::LayerContent::Smart(s) => {
                s.source.compact();
                if let Some(c) = s.cache.as_mut() {
                    c.compact();
                }
            }
            _ => {}
        }
        if let Some(m) = l.mask.as_mut() {
            m.tiles.compact();
        }
    });
}

/// Bytes of pixel data referenced by all layers and masks.
pub fn storage_bytes(doc: &Document) -> usize {
    let mut total = 0;
    doc.for_each_layer(|l| {
        if let Some(s) = l.raster_store() {
            total += s.byte_size();
        }
        if let Some(m) = &l.mask {
            total += m.tiles.byte_size();
        }
    });
    total
}

/// Owns the live document and its history.
pub struct Editor {
    doc: Document,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    pub history_limit: usize,
    /// Rough cap on the bytes the undo stack may keep alive; the oldest
    /// steps are dropped first. At least one step is always kept.
    pub history_memory_limit: usize,
    /// Key of the open coalescing run, if any (see [`Editor::execute_coalescing`]).
    coalesce_key: Option<String>,
    /// Area changed by the last successful edit/undo/redo; `None` = whole canvas.
    last_affected: Option<lumenply_tiles::Rect>,
    /// Layer targeted by the last successful edit (see [`Command::target_layer`]).
    last_target: Option<LayerId>,
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        Editor {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            history_limit: 100,
            history_memory_limit: 1 << 30, // 1 GiB
            coalesce_key: None,
            last_affected: None,
            last_target: None,
        }
    }

    /// Canvas area touched by the most recent change (see [`Command::affected`]).
    pub fn last_affected(&self) -> Option<lumenply_tiles::Rect> {
        self.last_affected
    }

    /// Layer the most recent edit targeted, when it could name one. Reset
    /// to `None` by undo, redo and jumps.
    pub fn last_target_layer(&self) -> Option<LayerId> {
        self.last_target
    }

    /// Estimated bytes the undo stack keeps alive beyond the live document.
    pub fn history_bytes(&self) -> usize {
        self.undo.iter().map(|s| s.bytes).sum()
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// Apply a command, recording an undo step. On error the document is
    /// left untouched.
    pub fn execute(&mut self, cmd: &dyn Command) -> EditResult {
        self.coalesce_key = None;
        self.push_command(cmd)
    }

    /// Like [`Editor::execute`], but consecutive calls with the same `key`
    /// share one undo step. Use for slider drags: every tick updates the
    /// document, yet the whole drag undoes in one go. Call
    /// [`Editor::end_coalescing`] when the drag ends.
    pub fn execute_coalescing(&mut self, cmd: &dyn Command, key: &str) -> EditResult {
        if self.coalesce_key.as_deref() == Some(key) {
            let mut next = self.doc.clone();
            cmd.apply(&mut next)?;
            locks::enforce(&self.doc, &mut next, cmd)?;
            compact_storage(&mut next);
            self.last_affected = cmd.affected(&self.doc);
            self.last_target = cmd.target_layer();
            // The whole drag undoes in one go, so its undo step covers
            // every tick so far, and its memory estimate follows the
            // moving document.
            let bytes = self.undo.last().map(|s| delta_bytes(&s.doc, &next));
            if let (Some(s), Some(b)) = (self.undo.last_mut(), bytes) {
                s.affected = union_opt(s.affected, self.last_affected);
                s.bytes = b;
            }
            self.doc = next;
            Ok(())
        } else {
            self.push_command(cmd)?;
            self.coalesce_key = Some(key.to_string());
            Ok(())
        }
    }

    /// Close the current coalescing run; the next edit starts a new undo step.
    pub fn end_coalescing(&mut self) {
        self.coalesce_key = None;
    }

    fn push_command(&mut self, cmd: &dyn Command) -> EditResult {
        let mut next = self.doc.clone();
        cmd.apply(&mut next)?;
        locks::enforce(&self.doc, &mut next, cmd)?;
        compact_storage(&mut next);
        self.last_affected = cmd.affected(&self.doc);
        self.last_target = cmd.target_layer();
        let prev = std::mem::replace(&mut self.doc, next);
        let bytes = delta_bytes(&prev, &self.doc);
        self.undo.push(Snapshot {
            label: cmd.label(),
            doc: prev,
            affected: self.last_affected,
            bytes,
        });
        while self.undo.len() > self.history_limit {
            self.undo.remove(0);
        }
        while self.undo.len() > 1 && self.history_bytes() > self.history_memory_limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(())
    }

    /// Returns the label of the undone command.
    pub fn undo(&mut self) -> Option<String> {
        self.coalesce_key = None;
        self.last_target = None;
        let snap = self.undo.pop()?;
        self.last_affected = snap.affected;
        let current = std::mem::replace(&mut self.doc, snap.doc);
        self.redo.push(Snapshot {
            label: snap.label.clone(),
            doc: current,
            affected: snap.affected,
            bytes: snap.bytes,
        });
        Some(snap.label)
    }

    /// Returns the label of the redone command.
    pub fn redo(&mut self) -> Option<String> {
        self.coalesce_key = None;
        self.last_target = None;
        let snap = self.redo.pop()?;
        self.last_affected = snap.affected;
        let current = std::mem::replace(&mut self.doc, snap.doc);
        self.undo.push(Snapshot {
            label: snap.label.clone(),
            doc: current,
            affected: snap.affected,
            bytes: snap.bytes,
        });
        Some(snap.label)
    }

    /// Undo or redo until exactly `steps` history entries remain applied.
    /// Afterwards [`Editor::last_affected`] covers every step crossed.
    pub fn jump_to(&mut self, steps: usize) {
        let mut acc: Option<Option<lumenply_tiles::Rect>> = None;
        while self.undo.len() > steps && self.undo().is_some() {
            acc = Some(match acc {
                None => self.last_affected,
                Some(a) => union_opt(a, self.last_affected),
            });
        }
        while self.undo.len() < steps && self.redo().is_some() {
            acc = Some(match acc {
                None => self.last_affected,
                Some(a) => union_opt(a, self.last_affected),
            });
        }
        if let Some(a) = acc {
            self.last_affected = a;
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Labels of the undo stack, oldest first.
    pub fn history(&self) -> Vec<&str> {
        self.undo.iter().map(|s| s.label.as_str()).collect()
    }

    /// Labels of steps that were undone and can be redone, in redo order.
    pub fn redo_history(&self) -> Vec<&str> {
        self.redo.iter().rev().map(|s| s.label.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::commands::*;
    use super::*;
    use lumenply_tiles::Rgba;

    #[test]
    fn undo_and_redo_restore_pixels() {
        let mut ed = Editor::new(Document::new(64, 64));
        ed.execute(&AddPixelLayer::new("Paint")).unwrap();
        let id = ed.doc().layers()[0].id;

        let stroke = PaintStroke {
            layer: id,
            brush: Brush {
                radius: 4.0,
                hardness: 1.0,
                color: [1.0, 0.0, 0.0, 1.0],
                spacing: 0.25,
                jitter: 0.0,
                mode: BrushMode::Paint,
            },
            points: vec![StrokePoint::new(10.0, 10.0, 1.0)],
        };
        ed.execute(&stroke).unwrap();
        let red = |ed: &Editor| ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(10, 10);
        assert!(red(&ed).a > 0.99);

        assert_eq!(ed.undo().as_deref(), Some("Paint stroke"));
        assert_eq!(red(&ed), Rgba::TRANSPARENT);
        assert_eq!(ed.redo().as_deref(), Some("Paint stroke"));
        assert!(red(&ed).a > 0.99);

        assert_eq!(ed.undo().as_deref(), Some("Paint stroke"));
        assert_eq!(ed.undo().as_deref(), Some("Add layer 'Paint'"));
        assert!(ed.doc().layers().is_empty());
        assert!(ed.undo().is_none());
    }

    #[test]
    fn failed_command_leaves_document_and_history_alone() {
        let mut ed = Editor::new(Document::new(8, 8));
        let err = ed.execute(&SetOpacity {
            layer: 42,
            opacity: 0.5,
        });
        assert!(matches!(err, Err(EditError::NoLayer(42))));
        assert!(!ed.can_undo());
    }

    #[test]
    fn coalescing_merges_a_drag_into_one_undo_step() {
        let mut ed = Editor::new(Document::new(8, 8));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        for i in 1..=10 {
            ed.execute_coalescing(
                &SetOpacity {
                    layer: id,
                    opacity: 1.0 - i as f32 * 0.05,
                },
                "opacity",
            )
            .unwrap();
        }
        assert_eq!(
            ed.history().len(),
            2,
            "one step for the layer, one for the whole drag"
        );
        assert!((ed.doc().layer(id).unwrap().opacity - 0.5).abs() < 1e-6);

        ed.end_coalescing();
        ed.execute_coalescing(
            &SetOpacity {
                layer: id,
                opacity: 0.25,
            },
            "opacity",
        )
        .unwrap();
        assert_eq!(ed.history().len(), 3, "a new run starts a new step");

        ed.undo();
        assert!((ed.doc().layer(id).unwrap().opacity - 0.5).abs() < 1e-6);
        ed.undo();
        assert_eq!(ed.doc().layer(id).unwrap().opacity, 1.0);
    }

    #[test]
    fn jump_to_walks_the_history_both_ways() {
        let mut ed = Editor::new(Document::new(8, 8));
        for i in 0..4 {
            ed.execute(&AddPixelLayer::new(format!("L{i}"))).unwrap();
        }
        ed.jump_to(1);
        assert_eq!(ed.doc().layer_count(), 1);
        assert_eq!(
            ed.redo_history(),
            vec!["Add layer 'L1'", "Add layer 'L2'", "Add layer 'L3'"]
        );
        ed.jump_to(3);
        assert_eq!(ed.doc().layer_count(), 3);
        ed.jump_to(10);
        assert_eq!(ed.doc().layer_count(), 4);
    }

    #[test]
    fn paint_reports_its_affected_area() {
        let mut ed = Editor::new(Document::new(500, 500));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        assert!(
            ed.last_affected().is_none(),
            "adding a layer: anything may change"
        );
        let id = ed.doc().layers()[0].id;
        ed.execute(&PaintStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                ..Brush::default()
            },
            points: vec![
                StrokePoint::new(100.0, 100.0, 1.0),
                StrokePoint::new(140.0, 120.0, 1.0),
            ],
        })
        .unwrap();
        let r = ed.last_affected().unwrap();
        assert!(r.contains(100, 100) && r.contains(140, 120));
        assert!(r.contains(89, 89) && !r.contains(60, 60), "{r:?}");
    }

    #[test]
    fn edits_are_stored_compactly_but_read_back_exactly_enough() {
        let mut ed = Editor::new(Document::new(300, 300));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        ed.execute(&Fill {
            layer: id,
            color: [0.2, 0.4, 0.6, 1.0],
        })
        .unwrap();
        let store = ed.doc().layer(id).unwrap().pixels().unwrap();
        assert!(store.coords().all(|c| store.tile(c).unwrap().is_compact()));
        assert_eq!(storage_bytes(ed.doc()), 4 * lumenply_tiles::TILE_PIXELS * 8);
        let p = store.get_pixel(150, 150).to_straight();
        assert!((p[0] - 0.2).abs() < 1e-4 && (p[2] - 0.6).abs() < 1e-4);
        // The undo snapshot still composites correctly after compaction.
        ed.undo();
        assert!(ed.doc().layer(id).unwrap().pixels().unwrap().is_empty());
        ed.redo();
        assert!(
            (ed.doc()
                .layer(id)
                .unwrap()
                .pixels()
                .unwrap()
                .get_pixel(10, 10)
                .to_straight()[1]
                - 0.4)
                .abs()
                < 1e-4
        );
    }

    #[test]
    fn undo_redo_and_jump_report_affected_areas() {
        let mut ed = Editor::new(Document::new(500, 500));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let dab = |x: f32, y: f32| PaintStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(x, y, 1.0)],
        };
        ed.execute(&dab(100.0, 100.0)).unwrap();
        ed.execute(&dab(300.0, 300.0)).unwrap();

        // Undoing a step dirties exactly that step's area.
        ed.undo();
        let r = ed.last_affected().expect("undo of a stroke is local");
        assert!(r.contains(300, 300) && !r.contains(100, 100), "{r:?}");
        ed.redo();
        let r = ed.last_affected().expect("redo of a stroke is local");
        assert!(r.contains(300, 300) && !r.contains(100, 100), "{r:?}");

        // Jumping across several steps unions their areas.
        ed.jump_to(1);
        let r = ed.last_affected().expect("both strokes are local");
        assert!(r.contains(100, 100) && r.contains(300, 300), "{r:?}");

        // A step whose command cannot bound its change poisons to "anything".
        ed.jump_to(3);
        ed.undo(); // stroke at (300, 300)
        ed.undo(); // stroke at (100, 100)
        ed.undo(); // AddPixelLayer: affected() is None
        assert!(ed.last_affected().is_none());

        // A coalesced drag undoes as the union of its ticks' areas.
        let mut ed = Editor::new(Document::new(500, 500));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let dab = |x: f32, y: f32| PaintStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(x, y, 1.0)],
        };
        ed.execute_coalescing(&dab(50.0, 50.0), "drag").unwrap();
        ed.execute_coalescing(&dab(400.0, 400.0), "drag").unwrap();
        ed.undo();
        let r = ed.last_affected().expect("coalesced strokes are local");
        assert!(r.contains(50, 50) && r.contains(400, 400), "{r:?}");
    }

    #[test]
    fn set_pass_through_is_group_only_and_undoable() {
        let mut ed = Editor::new(Document::new(8, 8));
        ed.execute(&AddPixelLayer::new("px")).unwrap();
        let px = ed.doc().layers()[0].id;
        assert!(matches!(
            ed.execute(&SetPassThrough {
                layer: px,
                pass_through: true
            }),
            Err(EditError::NotGroup(_))
        ));
        ed.execute(&GroupLayers {
            layers: vec![px],
            name: "g".into(),
        })
        .unwrap();
        let g = ed.doc().layers()[0].id;
        ed.execute(&SetPassThrough {
            layer: g,
            pass_through: true,
        })
        .unwrap();
        assert!(ed.doc().layer(g).unwrap().pass_through);
        ed.undo();
        assert!(!ed.doc().layer(g).unwrap().pass_through);
    }

    #[test]
    fn history_limit_is_enforced() {
        let mut ed = Editor::new(Document::new(8, 8));
        ed.history_limit = 3;
        for i in 0..5 {
            ed.execute(&AddPixelLayer::new(format!("L{i}"))).unwrap();
        }
        assert_eq!(ed.history().len(), 3);

        // Lowering the limit takes full effect on the next edit, not one
        // entry at a time.
        ed.history_limit = 10;
        for i in 5..12 {
            ed.execute(&AddPixelLayer::new(format!("L{i}"))).unwrap();
        }
        assert_eq!(ed.history().len(), 10);
        ed.history_limit = 2;
        ed.execute(&AddPixelLayer::new("last")).unwrap();
        assert_eq!(ed.history().len(), 2);
    }

    #[test]
    fn history_memory_limit_drops_oldest_steps() {
        // Each fill rewrites the full 600×600 canvas: 9 compact tiles, about
        // 4.5 MB per undo step.
        let mut ed = Editor::new(Document::new(600, 600));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        let step = 9 * lumenply_tiles::TILE_PIXELS * 8;
        ed.history_memory_limit = 3 * step + step / 2; // room for ~3 fills
        for i in 0..6 {
            ed.execute(&Fill {
                layer: id,
                color: [i as f32 / 10.0, 0.5, 0.5, 1.0],
            })
            .unwrap();
        }
        assert!(
            ed.history_bytes() <= ed.history_memory_limit,
            "{} > {}",
            ed.history_bytes(),
            ed.history_memory_limit
        );
        let len = ed.history().len();
        assert!((2..=4).contains(&len), "kept {len} steps");
        // The newest steps survive; undo still works.
        assert_eq!(ed.undo().as_deref(), Some("Fill"));

        // A tiny limit still keeps one undo step, even when it is heavy.
        // (The first two steps cost nothing: their snapshots hold empty
        // states. Only the second fill makes the first fill's tiles unique
        // to a snapshot.)
        let mut ed = Editor::new(Document::new(600, 600));
        ed.history_memory_limit = 1;
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        for c in [0.3, 0.7] {
            ed.execute(&Fill {
                layer: id,
                color: [c, 0.2, 0.3, 1.0],
            })
            .unwrap();
        }
        assert_eq!(ed.history().len(), 1);
        assert!(ed.can_undo());
    }
}
