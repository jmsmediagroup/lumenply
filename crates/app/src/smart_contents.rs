//! Smart object contents, as in Photoshop: Edit Contents opens the
//! untouched source pixels in a tab of their own, and Save there (Cmd+S,
//! or Save in the close prompt) writes the flattened result back into the
//! smart object, which keeps its transform. Replace Contents loads an
//! image file in place of the source.

use super::*;
use lumenply_core::smart_contents::ReplaceSmartSource;
use lumenply_tiles::TileStore;

/// Where a contents tab saves to: the document (by tab key) and smart
/// layer it came from, and where its source sat.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SmartLink {
    pub(crate) parent: u64,
    pub(crate) layer: LayerId,
    pub(crate) origin: (i32, i32),
    pub(crate) name: String,
}

impl App {
    /// A fresh key for a newly opened document.
    pub(crate) fn alloc_doc_key(&mut self) -> u64 {
        self.next_doc_key += 1;
        self.next_doc_key
    }

    /// Opens the active smart object's source in a new tab.
    pub(crate) fn edit_smart_contents(&mut self) {
        let Some(id) = self.active else { return };
        let Some(layer) = self.editor.doc().layer(id) else {
            return;
        };
        let Some(sm) = layer.smart_layer() else { return };
        // The painted pixels, not the whole tiles they sit in.
        let b = lumenply_core::snap::painted_bounds(&sm.source).unwrap_or(Rect::new(0, 0, 1, 1));
        let raster = sm.source.to_raster(b);
        let name = layer.name.clone();
        let mut ed = Editor::new(Document::new(b.w, b.h));
        let _ = ed.execute(&AddPixelLayer::from_raster("Contents", raster, 0, 0));
        let parent = self.doc_key;
        self.open_in_new_tab(Editor::new(ed.doc().clone()), None);
        self.untitled = format!("{name} (contents)");
        self.smart_link = Some(SmartLink {
            parent,
            layer: id,
            origin: (b.x, b.y),
            name,
        });
        self.status = "Editing the smart object's contents: Save (Cmd+S) updates it".into();
    }

    /// In a contents tab: flatten it into its smart object. False when this
    /// tab isn't one, or the document it belongs to is closed.
    pub(crate) fn save_smart_contents(&mut self) -> bool {
        let Some(link) = self.smart_link.clone() else {
            return false;
        };
        let flat = lumenply_render::composite_raster(self.editor.doc());
        let cmd = ReplaceSmartSource {
            layer: link.layer,
            source: TileStore::from_raster(&flat, link.origin.0, link.origin.1),
        };
        let Some(parent) = self.tabs.iter_mut().find(|t| t.doc_key == link.parent) else {
            self.status = format!(
                "The document '{}' came from is closed; save it as a file instead",
                link.name
            );
            return false;
        };
        match parent.editor.execute(&cmd) {
            Ok(()) => {
                self.saved_rev = self.editor.history().len();
                self.status = format!("Updated smart object '{}'", link.name);
                true
            }
            Err(e) => {
                self.status = format!("Could not update '{}': {e}", link.name);
                false
            }
        }
    }

    /// Save the live document: into its smart object for a contents tab,
    /// else to its file (asking for one the first time).
    pub(crate) fn save_live(&mut self) {
        if self.save_smart_contents() {
            return;
        }
        match self.path.clone() {
            Some(p) => self.save_path(&p.to_string_lossy()),
            None => self.pick_save(),
        }
    }

    /// Asks for an image and puts it in place of the active smart object's
    /// source (top-left where the old source sat; the transform stays).
    pub(crate) fn pick_replace_smart_contents(&mut self) {
        let mut exts: Vec<&str> = dialogs::IMAGE_EXT.to_vec();
        exts.extend_from_slice(lumenply_io::raw::RAW_EXT);
        if let Some(p) = self
            .file_dialog()
            .set_title("Replace contents")
            .add_filter("Images", &exts)
            .pick_file()
        {
            self.replace_smart_contents(&p.to_string_lossy());
        }
    }

    pub(crate) fn replace_smart_contents(&mut self, path: &str) {
        let Some(id) = self.active else { return };
        let Some(origin) = self
            .editor
            .doc()
            .layer(id)
            .and_then(|l| l.smart_layer())
            .map(|sm| lumenply_core::snap::painted_bounds(&sm.source).map_or((0, 0), |b| (b.x, b.y)))
        else {
            return;
        };
        match lumenply_io::load(path) {
            Ok(raster) => self.run(&ReplaceSmartSource {
                layer: id,
                source: TileStore::from_raster(&raster, origin.0, origin.1),
            }),
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    fn red_smart_doc() -> (App, LayerId) {
        let mut doc = Document::new(60, 40);
        let id = doc.add_pixel_layer("Logo");
        for y in 5..15 {
            for x in 5..15 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        app.run_menu_action("smart-object");
        (app, id)
    }

    #[test]
    fn editing_contents_and_saving_updates_the_smart_object() {
        let (mut app, id) = red_smart_doc();
        let parent_key = app.doc_key;
        app.run_menu_action("smart-edit");
        assert_eq!(app.untitled, "Logo (contents)");
        assert_eq!(
            (app.editor.doc().canvas().w, app.editor.doc().canvas().h),
            (10, 10)
        );
        // Paint the contents blue, then Save (Cmd+S's action).
        let layer = app.editor.doc().layers()[0].id;
        app.run(&SetSelection { selection: None });
        app.set_active(Some(layer));
        app.brush_rgb = [0.0, 0.0, 1.0];
        app.run_menu_action("fill");
        app.run_menu_action("save");
        assert!(app.status.starts_with("Updated smart object"), "{}", app.status);
        assert_eq!(
            app.editor.history().len(),
            app.saved_rev,
            "the contents tab counts as saved"
        );
        // Back in the original document the smart object is blue in place.
        let parent = app
            .tab_infos()
            .iter()
            .position(|(t, _)| t != "Logo (contents)")
            .unwrap();
        app.switch_tab(parent);
        assert_eq!(app.doc_key, parent_key);
        let px = app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(9, 9);
        assert!(px.b > 0.9 && px.r < 0.1, "{px:?}");
        assert_eq!(app.editor.history().last(), Some(&"Update smart object"));
    }

    #[test]
    fn edit_and_replace_need_a_smart_object() {
        let (mut app, _) = red_smart_doc();
        assert!(app.action_block("smart-edit").is_none());
        app.run_menu_action("rasterize");
        assert!(app.action_block("smart-edit").is_some());
        assert!(app.action_block("smart-replace").is_some());
    }
}
