//! Document resolution (pixels per inch): metadata that sets the print size
//! without touching pixels. `ResizeImage` (commands.rs) changes pixels and
//! resolution together; [`SetResolution`] changes only the resolution, as
//! Image ▸ Image Size does with Resample off.

use lumenply_doc::{Document, RESOLUTION_RANGE};

use crate::{Command, EditError, EditResult};

/// Reject resolutions outside [`RESOLUTION_RANGE`] (and NaN).
pub fn check_ppi(ppi: f32) -> EditResult {
    if RESOLUTION_RANGE.contains(&ppi) {
        Ok(())
    } else {
        Err(EditError::Invalid(format!(
            "resolution must be {}–{} ppi",
            RESOLUTION_RANGE.start(),
            RESOLUTION_RANGE.end()
        )))
    }
}

/// Set the print resolution in pixels per inch. No resampling: the pixels
/// stay, the print size becomes `width / ppi` inches.
pub struct SetResolution {
    pub ppi: f32,
}

impl Command for SetResolution {
    fn label(&self) -> String {
        "Resolution".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        check_ppi(self.ppi)?;
        if self.ppi == doc.resolution {
            return Err(EditError::Invalid("resolution is unchanged".into()));
        }
        doc.resolution = self.ppi;
        Ok(())
    }

    fn affected(&self, _doc: &Document) -> Option<lumenply_tiles::Rect> {
        // Nothing on the canvas changes.
        Some(lumenply_tiles::Rect::new(0, 0, 0, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ResizeImage;
    use crate::Editor;
    use lumenply_tiles::Rgba;

    fn doc_with_dot() -> Document {
        let mut doc = Document::new(60, 40);
        let id = doc.add_pixel_layer("p");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(10, 10, Rgba::WHITE);
        doc
    }

    #[test]
    fn new_documents_are_72_ppi() {
        let doc = Document::new(720, 360);
        assert_eq!(doc.resolution, 72.0);
        assert_eq!(doc.print_size_inches(), (10.0, 5.0));
    }

    #[test]
    fn set_resolution_keeps_pixels_and_undoes() {
        let mut ed = Editor::new(doc_with_dot());
        let before = ed.doc().clone();
        ed.execute(&SetResolution { ppi: 300.0 }).unwrap();
        assert_eq!(ed.doc().resolution, 300.0);
        assert_eq!((ed.doc().width, ed.doc().height), (60, 40));
        assert_eq!(ed.doc().print_size_inches(), (60.0 / 300.0, 40.0 / 300.0));
        let id = ed.doc().layers()[0].id;
        assert_eq!(
            ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(10, 10),
            before.layer(id).unwrap().pixels().unwrap().get_pixel(10, 10)
        );
        assert_eq!(ed.history().last().copied(), Some("Resolution"));
        ed.undo();
        assert_eq!(ed.doc().resolution, 72.0);
        ed.redo();
        assert_eq!(ed.doc().resolution, 300.0);
    }

    #[test]
    fn set_resolution_rejects_bad_values() {
        let mut doc = Document::new(4, 4);
        for bad in [0.0, -5.0, f32::NAN, 30_001.0] {
            assert!(SetResolution { ppi: bad }.apply(&mut doc).is_err(), "{bad}");
        }
        assert!(SetResolution { ppi: 72.0 }.apply(&mut doc).is_err(), "unchanged");
        assert_eq!(doc.resolution, 72.0);
    }

    #[test]
    fn resize_image_with_resolution() {
        // Resample on: 60×40 at 72 ppi → 250×(40·250/60) at 300 ppi keeps
        // the 0.833 in print width.
        let mut doc = doc_with_dot();
        ResizeImage {
            width: 250,
            height: 167,
            resolution: Some(300.0),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height, doc.resolution), (250, 167, 300.0));
        let (w_in, _) = doc.print_size_inches();
        assert!((w_in - 60.0 / 72.0).abs() < 0.001, "{w_in}");

        // Same pixel size with a new resolution: metadata only, no resample.
        let mut doc = doc_with_dot();
        let id = doc.layers()[0].id;
        ResizeImage {
            width: 60,
            height: 40,
            resolution: Some(150.0),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height, doc.resolution), (60, 40, 150.0));
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(10, 10),
            Rgba::WHITE
        );
        assert_eq!(doc.layer(id).unwrap().pixels().unwrap().get_pixel(11, 10).a, 0.0);

        // Nothing to change, or a bad resolution: refused, document intact.
        assert!(ResizeImage {
            width: 60,
            height: 40,
            resolution: Some(150.0),
        }
        .apply(&mut doc)
        .is_err());
        assert!(ResizeImage {
            width: 30,
            height: 20,
            resolution: Some(0.0),
        }
        .apply(&mut doc)
        .is_err());
        assert_eq!((doc.width, doc.resolution), (60, 150.0));

        // `None` keeps the resolution while resampling.
        ResizeImage {
            width: 30,
            height: 20,
            resolution: None,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height, doc.resolution), (30, 20, 150.0));
    }
}
