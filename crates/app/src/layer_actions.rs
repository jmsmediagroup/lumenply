//! Photoshop-standard layer operations in the shell: duplicate, merge
//! down / merge group / merge clipping mask, merge visible, flatten and
//! stamp visible. The commands live in `lumenply_core::layer_ops`; this
//! module picks the target layer, keeps the active layer pointing at the
//! result, and says why an operation can't run.

use super::*;
use lumenply_core::layer_ops::{self as ops, MergeKind};

impl App {
    /// Duplicate the active layer directly above itself and make the copy
    /// active (Layer ▸ Duplicate, Ctrl/Cmd+J without a selection).
    pub(crate) fn duplicate_active(&mut self) {
        let Some(layer) = self.active else { return };
        let copy = self.editor.doc().next_id();
        self.run(&ops::DuplicateLayer { layer });
        if self.editor.doc().layer(copy).is_some() {
            self.set_active(Some(copy));
            self.status = format!(
                "Duplicated as \"{}\"",
                self.active_layer().map_or("", |l| l.name.as_str())
            );
        }
    }

    /// The action Ctrl/Cmd+J runs: "layer via copy" with a selection,
    /// "duplicate layer" without one.
    pub(crate) fn cmd_j_action(&self) -> &'static str {
        if self.editor.doc().selection.is_some() {
            "layer-via-copy"
        } else {
            "duplicate-layer"
        }
    }

    /// What Ctrl/Cmd+E would do with the active layer, or why it can't.
    pub(crate) fn merge_kind(&self) -> Result<MergeKind, &'static str> {
        let id = self.active.ok_or("Select a layer first")?;
        ops::merge_down_kind(self.editor.doc(), id)
    }

    /// The Merge Down menu label for the active layer: "Merge down",
    /// "Merge group" or "Merge clipping mask", as in Photoshop.
    pub(crate) fn merge_label(&self) -> &'static str {
        self.merge_kind().map_or("Merge down", MergeKind::label)
    }

    /// Ctrl/Cmd+E: merge the active layer down (or its group, or the
    /// layers clipped to it) and keep the result active.
    pub(crate) fn merge_down_active(&mut self) {
        let Some(layer) = self.active else { return };
        let Ok(kind) = self.merge_kind() else { return };
        let result = match kind {
            MergeKind::Down => {
                let doc = self.editor.doc();
                let list = match doc.parent_of(layer) {
                    None => doc.layers(),
                    Some(p) => doc.layer(p).and_then(|g| g.children()).unwrap_or(&[]),
                };
                let i = list.iter().position(|l| l.id == layer).unwrap_or(0);
                list.get(i.wrapping_sub(1)).map_or(layer, |l| l.id)
            }
            MergeKind::Group | MergeKind::ClippingMask => layer,
        };
        self.run(&ops::MergeDown { layer });
        if self.editor.doc().layer(result).is_some() {
            self.set_active(Some(result));
        }
    }

    /// Shift+Ctrl/Cmd+E: merge every visible layer into one.
    pub(crate) fn merge_visible(&mut self) {
        let result = self
            .editor
            .doc()
            .layers()
            .iter()
            .find(|l| l.visible)
            .map(|l| l.id);
        self.run(&ops::MergeVisible);
        if result.is_some_and(|id| self.editor.doc().layer(id).is_some()) {
            self.set_active(result);
        }
    }

    /// Layer ▸ Flatten image: one opaque "Background" layer.
    pub(crate) fn flatten_image(&mut self) {
        let id = self.editor.doc().next_id();
        self.run(&ops::FlattenImage);
        if self.editor.doc().layer(id).is_some() {
            self.set_active(Some(id));
        }
    }

    /// Shift+Alt+Ctrl/Cmd+E: the visible composite on a new layer on top.
    pub(crate) fn stamp_visible(&mut self) {
        let id = self.editor.doc().next_id();
        let n = self.editor.doc().layer_count() + 1;
        self.run(&ops::StampVisible {
            name: format!("Stamp {n}"),
        });
        if self.editor.doc().layer(id).is_some() {
            self.set_active(Some(id));
        }
    }

    /// Why one of this module's actions can't run (see
    /// [`App::action_block`]); `None` for ids it doesn't own.
    pub(crate) fn layer_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        let doc = self.editor.doc();
        Some(match id {
            "duplicate-layer" => self.active_layer().is_none().then_some("Select a layer first"),
            "merge-down" => self.merge_kind().err(),
            "merge-visible" => ops::merge_visible_block(doc),
            "flatten" => doc
                .layers()
                .is_empty()
                .then_some("There are no layers to flatten"),
            "stamp-visible" => ops::stamp_visible_block(doc),
            _ => return None,
        })
    }

    /// Run one of this module's actions; false for ids it doesn't own.
    pub(crate) fn run_layer_action(&mut self, id: &str) -> bool {
        match id {
            "duplicate-layer" => self.duplicate_active(),
            "merge-down" => self.merge_down_active(),
            "merge-visible" => self.merge_visible(),
            "flatten" => self.flatten_image(),
            "stamp-visible" => self.stamp_visible(),
            _ => return false,
        }
        true
    }

    /// Shortcut labels for this module's fixed keys; `None` for others.
    pub(crate) fn layer_action_keys(&self, ctx: &egui::Context, id: &str) -> Option<String> {
        use egui::Modifiers as M;
        match id {
            // Ctrl/Cmd+J duplicates when nothing is selected (with a
            // selection it is "layer via copy"); it follows that binding.
            "duplicate-layer" => Some(session::chord_label(ctx, &self.prefs, "layer-via-copy")),
            "stamp-visible" => Some(shortcut_text(ctx, M::COMMAND | M::SHIFT | M::ALT, Key::E)),
            _ => None,
        }
    }

    /// The Layer menu's merge section.
    pub(crate) fn merge_menu_items(&mut self, ui: &mut egui::Ui) {
        let label = self.merge_label();
        self.act(ui, label, "merge-down");
        self.act(ui, "Merge visible", "merge-visible");
        self.act(ui, "Stamp visible to new layer", "stamp-visible");
        self.act(ui, "Flatten image", "flatten");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app for a command line, with autosave and the crash-recovery
    /// prompt kept out of the way (see `a11y_tests::launch`).
    fn launch(args: &[String]) -> App {
        let mut app = App::launch(args);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        app
    }

    fn doc_app() -> App {
        let mut app = launch(&[]);
        app.open_in_new_tab(blank(64, 64), None);
        app
    }

    #[test]
    fn duplicate_makes_the_copy_active_and_cmd_j_picks_by_selection() {
        let mut app = doc_app();
        app.add_pixel_layer();
        let orig = app.active.unwrap();
        app.run_menu_action("fill");
        assert_eq!(app.action_block("duplicate-layer"), None);
        app.run_menu_action("duplicate-layer");
        let copy = app.active.unwrap();
        assert_ne!(copy, orig);
        let doc = app.editor.doc();
        let name = &doc.layer(orig).unwrap().name;
        assert_eq!(doc.layer(copy).unwrap().name, format!("{name} copy"));
        let ids: Vec<LayerId> = doc.layers().iter().map(|l| l.id).collect();
        let at = ids.iter().position(|i| *i == orig).unwrap();
        assert_eq!(ids[at + 1], copy, "directly above the original");
        assert_eq!(app.cmd_j_action(), "duplicate-layer");
        app.run_menu_action("select-all");
        assert_eq!(app.cmd_j_action(), "layer-via-copy");
    }

    #[test]
    fn merge_actions_block_with_reasons_and_keep_the_result_active() {
        let mut app = doc_app();
        // The blank document has one white "Background" layer.
        let bg = app.active.unwrap();
        assert_eq!(
            app.action_block("merge-down"),
            Some("Nothing below to merge into")
        );
        assert_eq!(
            app.action_block("merge-visible"),
            Some("Only one layer is visible")
        );
        app.set_active(None);
        assert_eq!(app.action_block("merge-down"), Some("Select a layer first"));
        assert_eq!(app.action_block("duplicate-layer"), Some("Select a layer first"));
        app.set_active(Some(bg));
        app.add_pixel_layer();
        assert_eq!(app.action_block("merge-down"), None);
        assert_eq!(app.merge_label(), "Merge down");
        app.run_menu_action("merge-down");
        assert_eq!(app.active, Some(bg), "the merged layer keeps the lower id");
        assert_eq!(app.editor.doc().layers().len(), 1);
        app.run_menu_action("stamp-visible");
        assert_eq!(app.editor.doc().layers().len(), 2);
        assert_eq!(app.action_block("merge-visible"), None);
        app.run_menu_action("merge-visible");
        assert_eq!(app.editor.doc().layers().len(), 1);
        assert_eq!(app.active, Some(bg));
        app.run_menu_action("flatten");
        let d = app.editor.doc();
        assert_eq!(d.layers().len(), 1);
        assert_eq!(d.layers()[0].name, "Background");
        assert_ne!(d.layers()[0].id, bg, "a new layer");
        assert_eq!(app.active, Some(d.layers()[0].id));
        assert_eq!(app.action_block("flatten"), None);
        app.run(&RemoveLayer {
            layer: d.layers()[0].id,
        });
        assert_eq!(
            app.action_block("flatten"),
            Some("There are no layers to flatten")
        );
        assert_eq!(app.action_block("stamp-visible"), Some("Nothing is visible"));
    }
}
