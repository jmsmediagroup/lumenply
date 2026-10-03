//! Menu actions for everyday Photoshop edits: Layer via Cut, Reselect,
//! Edit ▸ Stroke, Bring to Front / Send to Back, Image ▸ Duplicate and
//! loading a layer's pixels as a selection.

use super::*;
use crate::dialogs::note;
use lumenply_core::everyday::{LayerViaCut, Reselect, SelectLayerPixels, StrokeLocation, StrokeSelection};

impl App {
    /// Why an everyday action can't run (`Some(None)` = it can), or
    /// `None` when `id` isn't one of them.
    pub(crate) fn everyday_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        let doc = self.editor.doc();
        let selection = doc.selection.is_some();
        Some(match id {
            "layer-via-cut" if !self.active_is_pixel() => Some("Select a pixel layer first"),
            "layer-via-cut" => None,
            "reselect" if doc.last_selection.is_none() => Some("Nothing has been deselected yet"),
            "reselect" => None,
            "stroke-selection" if !selection => Some("Make a selection first"),
            "stroke-selection" if !self.active_is_pixel() => Some("Select a pixel layer first"),
            "stroke-selection" => None,
            "select-layer-pixels" if self.active_layer().and_then(|l| l.raster_store()).is_none() => {
                Some("Select a layer with pixels first")
            }
            "select-layer-pixels" => None,
            "layer-front" | "layer-back" if self.active_layer().is_none() => Some("Select a layer first"),
            "layer-front" | "layer-back" | "duplicate-doc" => None,
            _ => return None,
        })
    }

    /// Run an everyday action; false when `id` isn't one of them.
    pub(crate) fn run_everyday_action(&mut self, id: &str) -> bool {
        match id {
            "layer-via-cut" => {
                if let Some(layer) = self.active {
                    let next = self.editor.doc().next_id();
                    self.run(&LayerViaCut {
                        layer,
                        name: "Layer via cut".into(),
                    });
                    if self.editor.doc().layer(next).is_some() {
                        self.set_active(Some(next));
                    }
                }
            }
            "reselect" => self.run(&Reselect),
            "stroke-selection" => self.dialog = Some(Dialog::Stroke(3.0, StrokeLocation::Center, 100.0)),
            "select-layer-pixels" => {
                if let Some(layer) = self.active {
                    self.load_layer_pixels(layer, CombineOp::Replace);
                }
            }
            "layer-front" | "layer-back" => self.layer_to_end(id == "layer-front"),
            "duplicate-doc" => {
                let doc = self.editor.doc().clone();
                let title = self
                    .path
                    .as_ref()
                    .map(|p| file_name(&p.to_string_lossy()))
                    .unwrap_or_else(|| self.untitled.clone());
                let name = format!("{title} copy");
                self.open_in_new_tab(Editor::new(doc), None);
                self.untitled = name;
                // A duplicate is new work, never saved anywhere yet.
                self.saved_rev = usize::MAX;
                self.status = "Duplicated the document".into();
            }
            _ => return false,
        }
        true
    }

    /// Fixed (not rebindable) keys of the everyday actions.
    pub(crate) fn everyday_action_keys(&self, ctx: &egui::Context, id: &str) -> Option<String> {
        use egui::Modifiers as M;
        let both = M::COMMAND | M::SHIFT;
        let (m, k) = match id {
            "layer-front" => (both, Key::CloseBracket),
            "layer-back" => (both, Key::OpenBracket),
            _ => return None,
        };
        Some(shortcut_text(ctx, m, k))
    }

    /// Cmd-click on a layer thumbnail: its pixels as the selection (Shift
    /// adds, Alt subtracts, both intersect).
    pub(crate) fn load_layer_pixels(&mut self, layer: LayerId, op: CombineOp) {
        self.run(&SelectLayerPixels { layer, op });
    }

    /// Bring to Front / Send to Back among the layer's siblings.
    fn layer_to_end(&mut self, front: bool) {
        let Some(layer) = self.active else { return };
        let doc = self.editor.doc();
        fn find(list: &[Layer], id: LayerId, parent: Option<LayerId>) -> Option<(Option<LayerId>, usize)> {
            if list.iter().any(|l| l.id == id) {
                return Some((parent, list.len()));
            }
            list.iter()
                .filter_map(|l| l.children().map(|c| (l.id, c)))
                .find_map(|(gid, c)| find(c, id, Some(gid)))
        }
        let Some((parent, n)) = find(doc.layers(), layer, None) else {
            return;
        };
        let index = if front { n - 1 } else { 0 };
        self.run(&RelocateLayer { layer, parent, index });
    }

    /// The Stroke dialog's body: width, location and opacity; the colour is
    /// the foreground colour.
    pub(crate) fn stroke_dialog_ui(
        ui: &mut egui::Ui,
        width: &mut f32,
        location: &mut StrokeLocation,
        opacity: &mut f32,
    ) {
        let r = field_row(
            ui,
            "Width",
            egui::DragValue::new(width)
                .range(1.0..=250.0)
                .max_decimals(1)
                .suffix(" px"),
        );
        a11y_name(&r, "Stroke width");
        ui.horizontal(|ui| {
            row_label(ui, "Location", LABEL_W);
            segmented(
                ui,
                location,
                &[
                    (StrokeLocation::Inside, "Inside"),
                    (StrokeLocation::Center, "Center"),
                    (StrokeLocation::Outside, "Outside"),
                ],
            );
        });
        let r = field_row(
            ui,
            "Opacity",
            egui::DragValue::new(opacity)
                .range(1.0..=100.0)
                .max_decimals(0)
                .suffix("%"),
        );
        a11y_name(&r, "Stroke opacity");
        note(
            ui,
            "Strokes the selection's edge in the foreground colour, on the active layer.",
        );
    }

    /// Edit ▸ Stroke, confirmed.
    pub(crate) fn stroke_selection(&mut self, width: f32, location: StrokeLocation, opacity: f32) {
        let Some(layer) = self.active else { return };
        let [r, g, b, _] = linear_rgba(self.brush_rgb, 1.0);
        self.run(&StrokeSelection {
            layer,
            width,
            color: [r, g, b],
            opacity: opacity / 100.0,
            location,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::{Rect, Rgba};

    fn app_with_square() -> (App, LayerId) {
        let mut doc = Document::new(40, 40);
        doc.add_pixel_layer("below");
        let id = doc.add_pixel_layer("photo");
        for y in 10..30 {
            for x in 10..30 {
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
        (app, id)
    }

    #[test]
    fn everyday_actions_run_from_the_registry() {
        let (mut app, id) = app_with_square();
        // Layer via cut makes the new layer active.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 10, 10, 20))),
        });
        assert_eq!(app.action_block("layer-via-cut"), None);
        app.run_menu_action("layer-via-cut");
        assert_ne!(app.active, Some(id));
        assert_eq!(app.editor.doc().layers().len(), 3);
        // Deselect, then Reselect brings the rectangle back.
        app.run_menu_action("deselect");
        assert!(app.editor.doc().selection.is_none());
        app.run_menu_action("reselect");
        assert_eq!(app.editor.doc().selection.as_ref().unwrap().value(12, 12), 1.0);
        // Stroke through the dialog, in the foreground colour.
        app.set_active(Some(id));
        app.brush_rgb = [0.0, 0.0, 1.0];
        app.run_menu_action("stroke-selection");
        assert!(matches!(app.dialog, Some(Dialog::Stroke(..))));
        app.dialog = None;
        app.stroke_selection(2.0, StrokeLocation::Outside, 100.0);
        let p = app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(9, 15);
        assert!(p.b > 0.99 && p.a > 0.99 && p.r < 0.01, "{p:?}");
        // Send to back, then bring to front.
        app.run_menu_action("layer-back");
        assert_eq!(app.editor.doc().layers()[0].id, id);
        app.run_menu_action("layer-front");
        assert_eq!(app.editor.doc().layers().last().unwrap().id, id);
        // Load the layer's pixels as the selection.
        app.run_menu_action("deselect");
        app.run_menu_action("select-layer-pixels");
        let s = app.editor.doc().selection.clone().unwrap();
        assert_eq!((s.value(25, 25), s.value(5, 5)), (1.0, 0.0));
        // Duplicate the document into a new, unsaved tab.
        let tabs = app.tab_infos().len();
        app.run_menu_action("duplicate-doc");
        assert_eq!(app.tab_infos().len(), tabs + 1);
        assert_eq!(app.editor.doc().layers().len(), 3);
        assert!(app.any_unsaved());
    }
}
