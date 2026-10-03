//! Edits that carry a model's output (ADR 0028): Object Selection, Select
//! Subject and Remove Background compute a coverage map off the UI thread,
//! and it lands here as one ordinary undoable step.
//!
//! Model output differs slightly between machines and providers, so these
//! commands carry the pixels themselves, never the model call: replaying
//! one gives exactly the same selection or mask everywhere.

use std::sync::Arc;

use lumenply_doc::{CombineOp, Document, LayerId, Mask, Selection};
use lumenply_tiles::{Rect, Rgba, Tile, TILE_SIZE};

use crate::{Command, EditError, EditResult};

/// A full-resolution coverage map in canvas pixels: `alpha[y * width + x]`,
/// 0 (outside) to 1 (inside). Values outside that range are clamped and
/// NaN counts as 0.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    pub width: u32,
    pub height: u32,
    pub alpha: Arc<Vec<f32>>,
}

impl Matte {
    /// `None` when `alpha` doesn't hold exactly `width × height` values.
    pub fn new(width: u32, height: u32, alpha: Vec<f32>) -> Option<Matte> {
        (alpha.len() == width as usize * height as usize).then(|| Matte {
            width,
            height,
            alpha: Arc::new(alpha),
        })
    }

    /// Coverage at a pixel (0 outside the map).
    pub fn value(&self, x: i32, y: i32) -> f32 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0.0;
        }
        clean(self.alpha[y as usize * self.width as usize + x as usize])
    }

    /// True when no pixel is covered at all.
    pub fn is_empty(&self) -> bool {
        self.alpha.iter().all(|&v| clean(v) <= 0.0)
    }

    /// The map as a hide-all mask: tiles only where something is covered.
    pub fn to_mask(&self) -> Mask {
        let mut mask = Mask::hide_all();
        let canvas = Rect::new(0, 0, self.width, self.height);
        let w = self.width as usize;
        for c in canvas.tiles() {
            let area = c.rect().intersect(&canvas);
            let (ox, oy) = c.origin();
            let mut tile = Tile::new();
            let mut any = false;
            // One borrow per tile: `pixels_mut` is not for per-pixel calls.
            let px = tile.pixels_mut();
            for y in area.y..area.bottom() {
                let row = y as usize * w;
                for x in area.x..area.right() {
                    let v = clean(self.alpha[row + x as usize]);
                    if v > 0.0 {
                        any = true;
                        px[(y - oy) as usize * TILE_SIZE + (x - ox) as usize] = Rgba::new(v, v, v, v);
                    }
                }
            }
            if any {
                mask.tiles.insert(c, Arc::new(tile));
            }
        }
        mask
    }

    fn check_fits(&self, doc: &Document) -> EditResult {
        if (self.width, self.height) == (doc.width, doc.height) {
            Ok(())
        } else {
            Err(EditError::Invalid(format!(
                "the result is {}×{} but the image is now {}×{}",
                self.width, self.height, doc.width, doc.height
            )))
        }
    }
}

fn clean(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Combine a model's coverage map with the selection: Object Selection
/// ("Object selection") and Select ▸ Subject ("Select subject").
pub struct SelectFromMatte {
    pub matte: Matte,
    pub op: CombineOp,
    /// The history label, naming the feature that made the map.
    pub label: String,
}

impl Command for SelectFromMatte {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        self.matte.check_fits(doc)?;
        let shape = Selection::from_mask(&self.matte.to_mask());
        let mut sel = match self.op {
            CombineOp::Replace => Selection::none(),
            _ => doc.selection.take().unwrap_or_else(Selection::none),
        };
        sel.combine(&shape, self.op);
        let next = Some(sel).filter(|s| !s.is_empty());
        // An emptied selection is remembered for Select ▸ Reselect, as
        // Deselect does.
        if next.is_none() {
            if let Some(old) = doc.selection.take() {
                doc.last_selection = Some(old);
            }
        }
        doc.selection = next;
        Ok(())
    }
}

/// Layer ▸ Remove Background: hide everything the map leaves uncovered
/// through the layer's mask. The pixels stay; a layer that already has a
/// mask keeps what it hides too (the two are intersected).
pub struct MaskFromMatte {
    pub layer: LayerId,
    pub matte: Matte,
}

impl Command for MaskFromMatte {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Remove background".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        self.matte.check_fits(doc)?;
        let fresh = self.matte.to_mask();
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let mask = match l.mask.take() {
            Some(mut old) => {
                old.combine(&fresh, CombineOp::Intersect);
                old.enabled = true;
                old
            }
            None => fresh,
        };
        l.mask = Some(mask);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddMask, SetSelection};
    use crate::Editor;

    /// 300 × 20: wider than a tile, so maps cross a tile boundary. Covers
    /// columns `x0..x1` fully, with one half-covered column after them.
    fn band(x0: u32, x1: u32) -> Matte {
        let (w, h) = (300u32, 20u32);
        let mut a = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in x0..x1 {
                a[(y * w + x) as usize] = 1.0;
            }
            a[(y * w + x1) as usize] = 0.5;
        }
        Matte::new(w, h, a).unwrap()
    }

    fn editor() -> Editor {
        let mut ed = Editor::new(Document::new(300, 20));
        ed.execute(&crate::commands::AddPixelLayer::new("Layer 1"))
            .unwrap();
        ed
    }

    fn sel(ed: &Editor, x: i32) -> f32 {
        ed.doc().selection.as_ref().map_or(0.0, |s| s.value(x, 7))
    }

    #[test]
    fn a_matte_must_hold_one_value_per_pixel() {
        assert!(Matte::new(3, 2, vec![0.0; 6]).is_some());
        assert!(Matte::new(3, 2, vec![0.0; 5]).is_none());
        let m = Matte::new(2, 1, vec![f32::NAN, 7.0]).unwrap();
        assert_eq!((m.value(0, 0), m.value(1, 0), m.value(2, 0)), (0.0, 1.0, 0.0));
        assert!(Matte::new(2, 1, vec![0.0, -1.0]).unwrap().is_empty());
    }

    #[test]
    fn the_mask_holds_the_map_and_only_covered_tiles() {
        let m = band(250, 270).to_mask();
        assert_eq!(m.default, 0.0);
        assert_eq!(m.tiles.len(), 2, "both tiles the band crosses, nothing else");
        assert_eq!(
            (m.value(249, 3), m.value(250, 3), m.value(269, 3)),
            (0.0, 1.0, 1.0)
        );
        assert_eq!((m.value(270, 3), m.value(271, 3)), (0.5, 0.0));
        assert_eq!(band(10, 20).to_mask().tiles.len(), 1);
    }

    #[test]
    fn selections_combine_with_the_map_as_the_tool_modes_say() {
        let mut ed = editor();
        let pick = |op, m: Matte| SelectFromMatte {
            matte: m,
            op,
            label: "Object selection".into(),
        };
        ed.execute(&pick(CombineOp::Replace, band(10, 20))).unwrap();
        assert_eq!(
            (sel(&ed, 9), sel(&ed, 10), sel(&ed, 19), sel(&ed, 20)),
            (0.0, 1.0, 1.0, 0.5)
        );
        ed.execute(&pick(CombineOp::Union, band(260, 280))).unwrap();
        assert_eq!((sel(&ed, 15), sel(&ed, 270), sel(&ed, 280)), (1.0, 1.0, 0.5));
        ed.execute(&pick(CombineOp::Subtract, band(5, 15))).unwrap();
        // 1 × (1 − 0.5) at the subtracted band's half-covered column.
        assert_eq!(
            (sel(&ed, 12), sel(&ed, 15), sel(&ed, 16), sel(&ed, 270)),
            (0.0, 0.5, 1.0, 1.0)
        );
        ed.execute(&pick(CombineOp::Intersect, band(0, 265))).unwrap();
        assert_eq!(
            (sel(&ed, 16), sel(&ed, 264), sel(&ed, 265), sel(&ed, 266)),
            (1.0, 1.0, 0.5, 0.0)
        );
        assert_eq!(
            ed.history(),
            vec![
                "Add layer 'Layer 1'",
                "Object selection",
                "Object selection",
                "Object selection",
                "Object selection"
            ]
        );
        // Replace drops whatever was there.
        ed.execute(&pick(CombineOp::Replace, band(100, 110))).unwrap();
        assert_eq!((sel(&ed, 16), sel(&ed, 105)), (0.0, 1.0));
        ed.undo();
        assert_eq!(
            (sel(&ed, 16), sel(&ed, 105)),
            (1.0, 0.0),
            "undo restores the previous selection"
        );
    }

    #[test]
    fn an_empty_result_deselects_and_keeps_the_old_selection_for_reselect() {
        let mut ed = editor();
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 50, 20))),
        })
        .unwrap();
        let none = Matte::new(300, 20, vec![0.0; 6000]).unwrap();
        ed.execute(&SelectFromMatte {
            matte: none,
            op: CombineOp::Replace,
            label: "Select subject".into(),
        })
        .unwrap();
        assert!(ed.doc().selection.is_none());
        let last = ed.doc().last_selection.as_ref().expect("kept for Reselect");
        assert_eq!(last.value(10, 10), 1.0);
    }

    #[test]
    fn a_map_of_another_size_is_refused() {
        let mut ed = editor();
        let wrong = Matte::new(299, 20, vec![1.0; 5980]).unwrap();
        let err = ed
            .execute(&SelectFromMatte {
                matte: wrong.clone(),
                op: CombineOp::Replace,
                label: "Select subject".into(),
            })
            .unwrap_err();
        assert!(err.to_string().contains("299×20"), "{err}");
        let layer = ed.doc().layers()[0].id;
        assert!(ed.execute(&MaskFromMatte { layer, matte: wrong }).is_err());
        assert_eq!(ed.history().len(), 1, "nothing recorded");
    }

    #[test]
    fn remove_background_masks_the_layer_and_keeps_its_pixels() {
        let mut ed = editor();
        let layer = ed.doc().layers()[0].id;
        ed.execute(&crate::commands::Fill {
            layer,
            color: [1.0, 0.0, 0.0, 1.0],
        })
        .unwrap();
        let before = ed.doc().layer(layer).unwrap().pixels().unwrap().get_pixel(5, 5);
        ed.execute(&MaskFromMatte {
            layer,
            matte: band(10, 20),
        })
        .unwrap();
        let l = ed.doc().layer(layer).unwrap();
        let m = l.mask.as_ref().expect("a mask");
        assert!(m.enabled);
        // Masks rest at 16 bits: the half-covered column reads 32768/65535.
        let half = 32768.0 / 65535.0;
        assert_eq!(
            (m.value(5, 5), m.value(15, 5), m.value(20, 5), m.value(200, 5)),
            (0.0, 1.0, half, 0.0)
        );
        assert_eq!(l.pixels().unwrap().get_pixel(5, 5), before, "pixels untouched");
        assert_eq!(ed.history().last().copied(), Some("Remove background"));
        ed.undo();
        assert!(ed.doc().layer(layer).unwrap().mask.is_none());
    }

    #[test]
    fn remove_background_keeps_what_an_existing_mask_hides() {
        let mut ed = editor();
        let layer = ed.doc().layers()[0].id;
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 16, 20))),
        })
        .unwrap();
        ed.execute(&AddMask { layer }).unwrap();
        ed.execute(&MaskFromMatte {
            layer,
            matte: band(10, 20),
        })
        .unwrap();
        let m = ed.doc().layer(layer).unwrap().mask.clone().unwrap();
        // Shown only where both the old mask (x < 16) and the map agree.
        assert_eq!(
            (m.value(9, 5), m.value(12, 5), m.value(15, 5), m.value(17, 5)),
            (0.0, 1.0, 1.0, 0.0)
        );
    }
}
