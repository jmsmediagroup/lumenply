//! Align and distribute layers by their content bounds, as Photoshop's
//! Move tool bar and Layer ▸ Align / Distribute do. One command moves every
//! layer involved, so the whole operation is one undo step. Moves are
//! whole-pixel translations (exact, no resampling).

use lumenply_doc::{Document, Layer, LayerContent, LayerId};
use lumenply_tiles::{Affine, Rect};

use crate::commands::TransformLayer;
use crate::locks::effective_locks;
use crate::{Command, EditError, EditResult};

/// Which edge (or centre line) to line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignEdge {
    Left,
    HCenter,
    Right,
    Top,
    VCenter,
    Bottom,
}

impl AlignEdge {
    pub const ALL: [AlignEdge; 6] = [
        AlignEdge::Left,
        AlignEdge::HCenter,
        AlignEdge::Right,
        AlignEdge::Top,
        AlignEdge::VCenter,
        AlignEdge::Bottom,
    ];

    fn horizontal(self) -> bool {
        matches!(self, AlignEdge::Left | AlignEdge::HCenter | AlignEdge::Right)
    }

    /// Twice the edge's coordinate in `r` (twice, so centres stay integral).
    fn coord2(self, r: Rect) -> i64 {
        let (x, y, w, h) = (r.x as i64, r.y as i64, r.w as i64, r.h as i64);
        match self {
            AlignEdge::Left => 2 * x,
            AlignEdge::HCenter => 2 * x + w,
            AlignEdge::Right => 2 * (x + w),
            AlignEdge::Top => 2 * y,
            AlignEdge::VCenter => 2 * y + h,
            AlignEdge::Bottom => 2 * (y + h),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AlignEdge::Left => "left edges",
            AlignEdge::HCenter => "horizontal centres",
            AlignEdge::Right => "right edges",
            AlignEdge::Top => "top edges",
            AlignEdge::VCenter => "vertical centres",
            AlignEdge::Bottom => "bottom edges",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignOp {
    /// Line every layer's edge up with the reference's.
    Align(AlignEdge),
    /// Space the layers' edges evenly between the outermost two (3+ layers).
    Distribute(AlignEdge),
}

/// Tight bounds of everything a layer paints (its own pixels, text or
/// smart-object render; a group's children), ignoring effects as Photoshop
/// does. `None` for empty layers, adjustments and live filters.
pub fn content_bounds(l: &Layer) -> Option<Rect> {
    match &l.content {
        LayerContent::Group(children) => children
            .iter()
            .filter_map(content_bounds)
            .reduce(|a, b| a.union(&b)),
        _ => l.raster_store()?.content_bounds(),
    }
}

/// Shift a layer (a group: everything in it) and its mask by whole pixels.
fn translate(doc: &mut Document, id: LayerId, dx: i32, dy: i32) -> EditResult {
    let l = doc.layer(id).ok_or(EditError::NoLayer(id))?;
    let t = Affine::translate(dx as f32, dy as f32);
    match &l.content {
        LayerContent::Group(children) => {
            let kids: Vec<LayerId> = children.iter().map(|c| c.id).collect();
            for k in kids {
                translate(doc, k, dx, dy)?;
            }
        }
        LayerContent::Adjustment(_) | LayerContent::Filter(_) => {}
        _ => {
            return TransformLayer {
                layer: id,
                transform: t,
            }
            .apply(doc)
        }
    }
    let l = doc.layer_mut(id).ok_or(EditError::NoLayer(id))?;
    if let Some(m) = l.mask.as_mut() {
        *m = lumenply_render::transform_mask(m, &t);
    }
    Ok(())
}

/// Align or distribute `layers` by their content bounds. With `to` set,
/// alignment is to that rectangle (the canvas or the selection, typically
/// for a single layer); otherwise to the bounds of all the layers
/// together. Layers without content are left alone. A position-locked
/// layer that would move refuses the whole command.
pub struct AlignLayers {
    pub layers: Vec<LayerId>,
    pub op: AlignOp,
    pub to: Option<Rect>,
}

/// Why `AlignLayers` with these layers can't do anything, if it can't.
pub fn align_block(
    doc: &Document,
    layers: &[LayerId],
    op: AlignOp,
    to: Option<Rect>,
) -> Option<&'static str> {
    let with_content = layers
        .iter()
        .filter(|id| doc.layer(**id).and_then(content_bounds).is_some())
        .count();
    match op {
        _ if with_content == 0 => Some("Select a layer with content first"),
        AlignOp::Align(_) if with_content < 2 && to.is_none() => Some("Select two or more layers"),
        AlignOp::Distribute(_) if with_content < 3 => Some("Select three or more layers"),
        _ => None,
    }
}

impl AlignLayers {
    /// The (dx, dy) each layer moves by.
    fn moves(&self, doc: &Document) -> Vec<(LayerId, i32, i32)> {
        // Each layer once, and never a layer inside another one listed
        // (moving the group already moves it).
        let mut ids: Vec<LayerId> = Vec::new();
        for id in &self.layers {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        let inside_other = |id: LayerId| {
            let mut cur = id;
            while let Some(p) = doc.parent_of(cur) {
                if ids.contains(&p) {
                    return true;
                }
                cur = p;
            }
            false
        };
        let items: Vec<(LayerId, Rect)> = ids
            .iter()
            .filter(|id| !inside_other(**id))
            .filter_map(|id| Some((*id, content_bounds(doc.layer(*id)?)?)))
            .collect();
        let shift = |edge: AlignEdge, id: LayerId, d2: i64| {
            // Half pixels (centres of odd-sized boxes) round toward −∞,
            // so the same inputs always land on the same pixel.
            let d = d2.div_euclid(2) as i32;
            if edge.horizontal() {
                (id, d, 0)
            } else {
                (id, 0, d)
            }
        };
        match self.op {
            AlignOp::Align(edge) => {
                let reference = self.to.unwrap_or_else(|| {
                    items
                        .iter()
                        .map(|(_, r)| *r)
                        .reduce(|a, b| a.union(&b))
                        .unwrap_or_default()
                });
                let target = edge.coord2(reference);
                items
                    .iter()
                    .map(|(id, r)| shift(edge, *id, target - edge.coord2(*r)))
                    .collect()
            }
            AlignOp::Distribute(edge) => {
                if items.len() < 3 {
                    return Vec::new();
                }
                let mut sorted = items.clone();
                sorted.sort_by_key(|(id, r)| (edge.coord2(*r), *id));
                let first = edge.coord2(sorted[0].1);
                let last = edge.coord2(sorted[sorted.len() - 1].1);
                let n = (sorted.len() - 1) as i64;
                sorted
                    .iter()
                    .enumerate()
                    .map(|(i, (id, r))| {
                        // Target in half pixels, rounded to whole pixels.
                        let t2 = first + ((last - first) * i as i64 + n / 2).div_euclid(n);
                        shift(edge, *id, t2 - edge.coord2(*r))
                    })
                    .collect()
            }
        }
    }
}

impl Command for AlignLayers {
    fn label(&self) -> String {
        match self.op {
            AlignOp::Align(e) => format!("Align {}", e.name()),
            AlignOp::Distribute(e) => format!("Distribute {}", e.name()),
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if let Some(why) = align_block(doc, &self.layers, self.op, self.to) {
            return Err(EditError::Invalid(why.into()));
        }
        let moves = self.moves(doc);
        for (id, dx, dy) in &moves {
            if (*dx, *dy) != (0, 0) && effective_locks(doc, *id).position {
                let name = doc.layer(*id).map_or("layer", |l| l.name.as_str());
                return Err(EditError::Invalid(format!(
                    "\"{name}\" is locked: its position is locked"
                )));
            }
        }
        for (id, dx, dy) in moves {
            if (dx, dy) != (0, 0) {
                translate(doc, id, dx, dy)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::*;
    use crate::locks::SetLayerLocks;
    use crate::Editor;
    use lumenply_doc::LayerLocks;
    use lumenply_tiles::{Raster, Rgba};

    fn square(ed: &mut Editor, x: i32, y: i32, w: u32, h: u32) -> LayerId {
        let id = ed.doc().next_id();
        let r = Raster::filled(w, h, Rgba::WHITE);
        ed.execute(&AddPixelLayer::from_raster(format!("L{id}"), r, x, y))
            .unwrap();
        id
    }

    fn bounds(ed: &Editor, id: LayerId) -> Rect {
        content_bounds(ed.doc().layer(id).unwrap()).unwrap()
    }

    #[test]
    fn align_lines_up_edges_and_centres_of_the_selection() {
        let mut ed = Editor::new(Document::new(400, 300));
        let a = square(&mut ed, 10, 20, 30, 30);
        let b = square(&mut ed, 100, 50, 50, 10);
        let c = square(&mut ed, 200, 5, 20, 80);
        let all = vec![a, b, c];
        let run = |ed: &mut Editor, edge| {
            ed.execute(&AlignLayers {
                layers: all.clone(),
                op: AlignOp::Align(edge),
                to: None,
            })
            .unwrap();
        };
        run(&mut ed, AlignEdge::Left);
        assert_eq!(
            [bounds(&ed, a).x, bounds(&ed, b).x, bounds(&ed, c).x],
            [10, 10, 10]
        );
        run(&mut ed, AlignEdge::Bottom);
        // The union spans y 5..85.
        assert_eq!(
            [
                bounds(&ed, a).bottom(),
                bounds(&ed, b).bottom(),
                bounds(&ed, c).bottom()
            ],
            [85, 85, 85]
        );
        run(&mut ed, AlignEdge::HCenter);
        // Union x 10..60 (widest is 50): centre 35.
        assert_eq!(bounds(&ed, a), Rect::new(20, 55, 30, 30));
        assert_eq!(bounds(&ed, b), Rect::new(10, 75, 50, 10));
        assert_eq!(bounds(&ed, c), Rect::new(25, 5, 20, 80));
        assert_eq!(ed.history().last().copied(), Some("Align horizontal centres"));
        // One undo step per alignment.
        ed.undo();
        assert_eq!(bounds(&ed, a).x, 10);
    }

    #[test]
    fn a_single_layer_aligns_to_the_given_rectangle() {
        let mut ed = Editor::new(Document::new(400, 300));
        let a = square(&mut ed, 10, 20, 30, 30);
        assert_eq!(
            align_block(ed.doc(), &[a], AlignOp::Align(AlignEdge::Left), None),
            Some("Select two or more layers")
        );
        let canvas = ed.doc().canvas();
        for (edge, want) in [
            (AlignEdge::Right, Rect::new(370, 20, 30, 30)),
            (AlignEdge::VCenter, Rect::new(370, 135, 30, 30)),
            (AlignEdge::HCenter, Rect::new(185, 135, 30, 30)),
        ] {
            ed.execute(&AlignLayers {
                layers: vec![a],
                op: AlignOp::Align(edge),
                to: Some(canvas),
            })
            .unwrap();
            assert_eq!(bounds(&ed, a), want, "{edge:?}");
        }
    }

    #[test]
    fn distribute_spaces_centres_evenly_and_keeps_the_outer_two() {
        let mut ed = Editor::new(Document::new(400, 300));
        let a = square(&mut ed, 0, 0, 20, 20); // centre 10
        let b = square(&mut ed, 30, 0, 10, 20); // centre 35
        let c = square(&mut ed, 200, 0, 40, 20); // centre 220
        let d = square(&mut ed, 50, 0, 20, 20); // centre 60
        assert_eq!(
            align_block(ed.doc(), &[a, b], AlignOp::Distribute(AlignEdge::HCenter), None),
            Some("Select three or more layers")
        );
        ed.execute(&AlignLayers {
            layers: vec![a, b, c, d],
            op: AlignOp::Distribute(AlignEdge::HCenter),
            to: None,
        })
        .unwrap();
        // Centres 10 … 220 in three equal steps of 70: 10, 80, 150, 220,
        // in the order the layers already stood (a, b, d, c).
        let centre = |id| {
            let r = bounds(&ed, id);
            r.x as f32 + r.w as f32 / 2.0
        };
        assert_eq!(
            [centre(a), centre(b), centre(d), centre(c)],
            [10.0, 80.0, 150.0, 220.0]
        );
        // Vertical positions untouched.
        assert_eq!(bounds(&ed, b).y, 0);
    }

    #[test]
    fn groups_move_whole_text_moves_by_anchor_and_locks_refuse() {
        let mut ed = Editor::new(Document::new(400, 300));
        let a = square(&mut ed, 100, 100, 10, 10);
        let b = square(&mut ed, 120, 140, 10, 10);
        let g = ed.doc().next_id();
        ed.execute(&GroupLayers {
            layers: vec![a, b],
            name: "G".into(),
        })
        .unwrap();
        let lone = square(&mut ed, 10, 10, 5, 5);
        ed.execute(&AlignLayers {
            layers: vec![g, a, lone],
            op: AlignOp::Align(AlignEdge::Left),
            to: None,
        })
        .unwrap();
        // The group moved as one (its child listed too moved only once).
        assert_eq!(bounds(&ed, a).x, 10);
        assert_eq!(bounds(&ed, b).x, 30);
        let t = ed.doc().next_id();
        ed.execute(&AddTextLayer {
            text: lumenply_doc::TextLayer::new("Hi", 300.0, 200.0, 20.0, [1.0; 4]),
            above: None,
        })
        .unwrap();
        let before = ed.doc().layer(t).unwrap().text_layer().unwrap().x;
        let tb = bounds(&ed, t);
        ed.execute(&AlignLayers {
            layers: vec![t, lone],
            op: AlignOp::Align(AlignEdge::Left),
            to: None,
        })
        .unwrap();
        let after = ed.doc().layer(t).unwrap().text_layer().unwrap().x;
        assert_eq!(after - before, (10 - tb.x) as f32, "text moves by its anchor");
        ed.execute(&SetLayerLocks {
            layer: lone,
            locks: LayerLocks {
                position: true,
                ..LayerLocks::NONE
            },
        })
        .unwrap();
        let err = ed
            .execute(&AlignLayers {
                layers: vec![g, lone],
                op: AlignOp::Align(AlignEdge::Right),
                to: None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("its position is locked"), "{err}");
    }
}
