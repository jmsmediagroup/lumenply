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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

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
