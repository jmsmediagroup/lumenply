//! Select ▸ Save Selection / Load Selection: keep selections under a name
//! in the document and bring them back, combined with the current one.

use crate::{Command, EditError, EditResult};
use lumenply_doc::channels::unique_name;
use lumenply_doc::{CombineOp, Document, SavedSelection, Selection};
use lumenply_tiles::Rect;

/// Keep the current selection under `name` (made unique).
pub struct SaveSelection {
    pub name: String,
}

impl Command for SaveSelection {
    fn label(&self) -> String {
        "Save selection".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .selection
            .as_ref()
            .ok_or_else(|| EditError::Invalid("there is no selection to save".into()))?;
        let base = if self.name.trim().is_empty() {
            "Selection"
        } else {
            self.name.trim()
        };
        let name = unique_name(base, &doc.saved_selections);
        let mask = sel.to_mask();
        doc.saved_selections.push(SavedSelection { name, mask });
        Ok(())
    }
}

/// Bring back saved selection `index`, optionally inverted, combined with
/// the current selection by `op`.
pub struct LoadSelection {
    pub index: usize,
    pub op: CombineOp,
    pub invert: bool,
}

impl Command for LoadSelection {
    fn label(&self) -> String {
        "Load selection".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let saved = doc
            .saved_selections
            .get(self.index)
            .ok_or_else(|| EditError::Invalid(format!("no saved selection {}", self.index)))?;
        let mut shape = Selection::from_mask(&saved.mask);
        if self.invert {
            shape.invert();
        }
        let mut sel = match self.op {
            CombineOp::Replace => Selection::none(),
            _ => doc.selection.take().unwrap_or_else(Selection::none),
        };
        sel.combine(&shape, self.op);
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

/// Forget saved selection `index`.
pub struct DeleteSavedSelection {
    pub index: usize,
}

impl Command for DeleteSavedSelection {
    fn label(&self) -> String {
        "Delete saved selection".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.index >= doc.saved_selections.len() {
            return Err(EditError::Invalid(format!("no saved selection {}", self.index)));
        }
        doc.saved_selections.remove(self.index);
        Ok(())
    }
}

/// Rename saved selection `index` (the Channels panel's double-click).
/// The name is trimmed and made unique among the other saved selections.
pub struct RenameSavedSelection {
    pub index: usize,
    pub name: String,
}

impl Command for RenameSavedSelection {
    fn label(&self) -> String {
        "Rename channel".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.index >= doc.saved_selections.len() {
            return Err(EditError::Invalid(format!("no saved selection {}", self.index)));
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err(EditError::Invalid("the channel needs a name".into()));
        }
        // Unique among the others: renaming "Sky" to "Sky" keeps it.
        let others: Vec<SavedSelection> = doc
            .saved_selections
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.index)
            .map(|(_, s)| SavedSelection {
                name: s.name.clone(),
                mask: lumenply_doc::Mask::hide_all(),
            })
            .collect();
        doc.saved_selections[self.index].name = unique_name(name, &others);
        Ok(())
    }
}

/// A channel of the composite image, for [`SelectionFromChannel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourChannel {
    Red,
    Green,
    Blue,
    /// The RGB composite's luminosity (Rec. 601 weights on the
    /// gamma-encoded values, as Photoshop loads it).
    Luminosity,
}

impl ColourChannel {
    /// The channel's value for a straight, linear colour over white (where
    /// the image is transparent a channel reads white, as it shows).
    pub fn value(self, straight: [f32; 4]) -> f32 {
        let [r, g, b, a] = straight;
        let enc = |v: f32| lumenply_doc::adjust::srgb_encode(v);
        let v = match self {
            ColourChannel::Red => enc(r),
            ColourChannel::Green => enc(g),
            ColourChannel::Blue => enc(b),
            ColourChannel::Luminosity => 0.299 * enc(r) + 0.587 * enc(g) + 0.114 * enc(b),
        };
        let a = a.clamp(0.0, 1.0);
        (v * a + (1.0 - a)).clamp(0.0, 1.0)
    }
}

/// Photoshop's Cmd-click on the RGB, Red, Green or Blue channel: the
/// composite's brightness in that channel becomes selection coverage
/// (white fully selected, black not), combined with the current selection
/// by `op`. The basis of luminosity masks.
pub struct SelectionFromChannel {
    pub channel: ColourChannel,
    pub op: CombineOp,
}

impl Command for SelectionFromChannel {
    fn label(&self) -> String {
        "Load channel as selection".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let flat = lumenply_render::composite_raster(doc);
        let mut mask = lumenply_doc::Mask::hide_all();
        for y in 0..flat.height {
            for x in 0..flat.width {
                let v = self.channel.value(flat.get(x, y).to_straight());
                if v > 0.0 {
                    mask.set_value(x as i32, y as i32, v);
                }
            }
        }
        let shape = Selection::from_mask(&mask);
        let mut sel = match self.op {
            CombineOp::Replace => Selection::none(),
            _ => doc.selection.take().unwrap_or_else(Selection::none),
        };
        sel.combine(&shape, self.op);
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    #[test]
    fn a_channel_loads_as_a_selection_of_its_brightness() {
        use lumenply_tiles::{Raster, Rgba};
        let mut px = Raster::new(4, 1);
        px.set(0, 0, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
        // Linear 0.2159 encodes to 0.5020 (sRGB 128).
        px.set(1, 0, Rgba::from_straight(0.2159, 0.2159, 0.2159, 1.0));
        // (2, 0) stays transparent: it reads white.
        px.set(3, 0, Rgba::from_straight(0.0, 0.0, 0.0, 1.0));
        let mut ed = Editor::new(Document::new(4, 1));
        ed.execute(&crate::commands::AddPixelLayer::from_raster("Bg", px, 0, 0))
            .unwrap();
        let load = |ed: &mut Editor, channel, op| {
            ed.execute(&SelectionFromChannel { channel, op }).unwrap();
            let s = ed.doc().selection.clone();
            (0..4)
                .map(|x| s.as_ref().map_or(0.0, |s| s.value(x, 0)))
                .collect::<Vec<f32>>()
        };
        let close = |got: Vec<f32>, want: [f32; 4]| {
            assert!(
                got.iter().zip(want).all(|(g, w)| (g - w).abs() < 2e-3),
                "{got:?} != {want:?}"
            );
        };
        close(
            load(&mut ed, ColourChannel::Red, CombineOp::Replace),
            [1.0, 0.502, 1.0, 0.0],
        );
        close(
            load(&mut ed, ColourChannel::Green, CombineOp::Replace),
            [0.0, 0.502, 1.0, 0.0],
        );
        close(
            load(&mut ed, ColourChannel::Luminosity, CombineOp::Replace),
            [0.299, 0.502, 1.0, 0.0],
        );
        // Combined: luminosity ∩ red keeps the smaller of the two.
        close(
            load(&mut ed, ColourChannel::Red, CombineOp::Intersect),
            [0.299, 0.502, 1.0, 0.0],
        );
        // Luminosity minus green: 0.299 · (1 − 0) at red, 0.502 · 0.498 at gray.
        load(&mut ed, ColourChannel::Luminosity, CombineOp::Replace);
        close(
            load(&mut ed, ColourChannel::Green, CombineOp::Subtract),
            [0.299, 0.250, 0.0, 0.0],
        );
        assert_eq!(ed.history().last().copied(), Some("Load channel as selection"));
        assert_eq!(ColourChannel::Blue.value([0.0, 0.0, 1.0, 0.5]), 1.0);
        assert_eq!(ColourChannel::Blue.value([1.0, 1.0, 0.0, 0.25]), 0.75);
    }

    #[test]
    fn renaming_a_channel_trims_keeps_it_unique_and_undoes() {
        let mut doc = Document::new(10, 10);
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 5, 5)));
        let mut ed = Editor::new(doc);
        ed.execute(&SaveSelection { name: "Sky".into() }).unwrap();
        ed.execute(&SaveSelection { name: "Tree".into() }).unwrap();
        let names = |ed: &Editor| -> Vec<String> {
            ed.doc().saved_selections.iter().map(|s| s.name.clone()).collect()
        };
        ed.execute(&RenameSavedSelection {
            index: 1,
            name: "  Hair  ".into(),
        })
        .unwrap();
        assert_eq!(names(&ed), ["Sky", "Hair"]);
        // Taking another channel's name gets a number, its own name doesn't.
        ed.execute(&RenameSavedSelection {
            index: 1,
            name: "Sky".into(),
        })
        .unwrap();
        assert_eq!(names(&ed), ["Sky", "Sky 2"]);
        ed.execute(&RenameSavedSelection {
            index: 0,
            name: "Sky".into(),
        })
        .unwrap();
        assert_eq!(names(&ed), ["Sky", "Sky 2"]);
        // The mask itself is untouched.
        assert_eq!(ed.doc().saved_selections[1].mask.value(2, 2), 1.0);
        assert_eq!(ed.doc().saved_selections[1].mask.value(7, 7), 0.0);
        assert_eq!(ed.history().len(), 5);
        ed.undo();
        ed.undo();
        assert_eq!(names(&ed), ["Sky", "Hair"]);
        assert!(ed
            .execute(&RenameSavedSelection {
                index: 0,
                name: "   ".into()
            })
            .is_err());
        assert!(ed
            .execute(&RenameSavedSelection {
                index: 9,
                name: "x".into()
            })
            .is_err());
    }

    fn at(ed: &Editor, x: i32, y: i32) -> f32 {
        ed.doc().selection.as_ref().map_or(0.0, |s| s.value(x, y))
    }

    #[test]
    fn a_saved_selection_comes_back_after_others_replace_it() {
        let mut doc = Document::new(100, 100);
        doc.selection = Some(Selection::rect(Rect::new(10, 10, 20, 20)));
        let mut ed = Editor::new(doc);
        ed.execute(&SaveSelection { name: "Box".into() }).unwrap();
        assert_eq!(ed.doc().saved_selections[0].name, "Box");
        // Another selection replaces it...
        ed.execute(&crate::commands::SetSelection {
            selection: Some(Selection::rect(Rect::new(60, 60, 10, 10))),
        })
        .unwrap();
        assert_eq!(at(&ed, 15, 15), 0.0);
        // ...and Load brings the box back.
        ed.execute(&LoadSelection {
            index: 0,
            op: CombineOp::Replace,
            invert: false,
        })
        .unwrap();
        assert_eq!(at(&ed, 15, 15), 1.0);
        assert_eq!(at(&ed, 65, 65), 0.0);
    }

    #[test]
    fn loading_combines_and_inverts() {
        let mut doc = Document::new(100, 100);
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 50, 100)));
        let mut ed = Editor::new(doc);
        ed.execute(&SaveSelection { name: "Left".into() }).unwrap();
        ed.execute(&crate::commands::SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 100, 50))),
        })
        .unwrap();
        // Top half ∩ left half = the top-left quarter.
        ed.execute(&LoadSelection {
            index: 0,
            op: CombineOp::Intersect,
            invert: false,
        })
        .unwrap();
        assert_eq!(
            (at(&ed, 25, 25), at(&ed, 75, 25), at(&ed, 25, 75)),
            (1.0, 0.0, 0.0)
        );
        // Inverted: the right half.
        ed.execute(&LoadSelection {
            index: 0,
            op: CombineOp::Replace,
            invert: true,
        })
        .unwrap();
        assert_eq!((at(&ed, 25, 25), at(&ed, 75, 25)), (0.0, 1.0));
    }

    #[test]
    fn names_are_unique_and_saving_needs_a_selection() {
        let mut doc = Document::new(10, 10);
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 5, 5)));
        let mut ed = Editor::new(doc);
        ed.execute(&SaveSelection { name: "".into() }).unwrap();
        ed.execute(&SaveSelection { name: "".into() }).unwrap();
        let names: Vec<_> = ed.doc().saved_selections.iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, ["Selection", "Selection 2"]);
        ed.execute(&DeleteSavedSelection { index: 0 }).unwrap();
        assert_eq!(ed.doc().saved_selections[0].name, "Selection 2");
        ed.undo();
        assert_eq!(ed.doc().saved_selections.len(), 2);
        let mut empty = Editor::new(Document::new(10, 10));
        assert!(empty.execute(&SaveSelection { name: "x".into() }).is_err());
    }
}
