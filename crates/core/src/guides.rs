//! Guide commands: add, move, remove and clear the document's ruler
//! guides. Guides never touch pixels, so every command here reports an
//! empty affected area (nothing to recomposite).

use lumenply_doc::{Document, Guide};
use lumenply_tiles::Rect;

use crate::{Command, EditError, EditResult};

/// Guides further than this outside the canvas are refused: they could
/// never be seen or snapped to, and PSD stores positions as i32 / 32.
const GUIDE_LIMIT: f32 = 1_000_000.0;

fn check_pos(pos: f32) -> EditResult {
    if pos.is_finite() && pos.abs() <= GUIDE_LIMIT {
        Ok(())
    } else {
        Err(EditError::Invalid(format!(
            "guide position {pos} is out of range"
        )))
    }
}

fn nothing_to_redraw() -> Option<Rect> {
    Some(Rect::default())
}

/// Add a guide (appended, so existing indices stay valid).
pub struct AddGuide {
    pub guide: Guide,
}

impl Command for AddGuide {
    fn label(&self) -> String {
        "New guide".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        nothing_to_redraw()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        check_pos(self.guide.pos)?;
        doc.guides.push(self.guide);
        Ok(())
    }
}

/// Move guide `index` to a new position along its axis.
pub struct MoveGuide {
    pub index: usize,
    pub pos: f32,
}

impl Command for MoveGuide {
    fn label(&self) -> String {
        "Move guide".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        nothing_to_redraw()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        check_pos(self.pos)?;
        let g = doc
            .guides
            .get_mut(self.index)
            .ok_or_else(|| EditError::Invalid(format!("no guide {}", self.index)))?;
        g.pos = self.pos;
        Ok(())
    }
}

/// Delete guide `index`.
pub struct RemoveGuide {
    pub index: usize,
}

impl Command for RemoveGuide {
    fn label(&self) -> String {
        "Delete guide".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        nothing_to_redraw()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.index >= doc.guides.len() {
            return Err(EditError::Invalid(format!("no guide {}", self.index)));
        }
        doc.guides.remove(self.index);
        Ok(())
    }
}

/// Delete every guide.
pub struct ClearGuides;

impl Command for ClearGuides {
    fn label(&self) -> String {
        "Clear guides".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        nothing_to_redraw()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if doc.guides.is_empty() {
            return Err(EditError::Invalid("there are no guides to clear".into()));
        }
        doc.guides.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    #[test]
    fn guides_add_move_remove_and_clear_with_undo() {
        let mut ed = Editor::new(Document::new(200, 100));
        ed.execute(&AddGuide {
            guide: Guide::vertical(50.0),
        })
        .unwrap();
        ed.execute(&AddGuide {
            guide: Guide::horizontal(25.5),
        })
        .unwrap();
        assert_eq!(
            ed.doc().guides,
            vec![Guide::vertical(50.0), Guide::horizontal(25.5)]
        );
        // Guides change no pixels: the redraw area is empty.
        assert_eq!(ed.last_affected(), Some(Rect::default()));

        ed.execute(&MoveGuide {
            index: 0,
            pos: 120.25,
        })
        .unwrap();
        assert_eq!(ed.doc().guides[0], Guide::vertical(120.25));
        assert_eq!(
            ed.doc().guides[1],
            Guide::horizontal(25.5),
            "the other guide stays"
        );

        ed.execute(&RemoveGuide { index: 1 }).unwrap();
        assert_eq!(ed.doc().guides, vec![Guide::vertical(120.25)]);

        ed.undo();
        assert_eq!(ed.doc().guides.len(), 2, "undo brings the removed guide back");
        ed.undo();
        assert_eq!(ed.doc().guides[0].pos, 50.0, "undo restores the old position");

        ed.execute(&ClearGuides).unwrap();
        assert!(ed.doc().guides.is_empty());
        assert_eq!(ed.history().last().copied(), Some("Clear guides"));
        ed.undo();
        assert_eq!(ed.doc().guides.len(), 2);
    }

    #[test]
    fn bad_guide_edits_are_refused_without_a_history_step() {
        let mut ed = Editor::new(Document::new(10, 10));
        assert!(ed.execute(&MoveGuide { index: 0, pos: 1.0 }).is_err());
        assert!(ed.execute(&RemoveGuide { index: 3 }).is_err());
        assert!(ed.execute(&ClearGuides).is_err());
        assert!(ed
            .execute(&AddGuide {
                guide: Guide::vertical(f32::NAN)
            })
            .is_err());
        assert!(ed
            .execute(&AddGuide {
                guide: Guide::horizontal(2.0e6)
            })
            .is_err());
        assert!(ed.doc().guides.is_empty());
        assert_eq!(ed.history().len(), 0);
    }
}
