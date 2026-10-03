//! Color Lookup in the app: the Properties section (the built-in looks,
//! "Load 3D LUT…", the table's name and size), the `load-lut` action and
//! the `lut:` screenshot tokens. Tables loaded from files are embedded in
//! the document; nothing keeps a path to the user's file.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::*;
use lumenply_doc::lut::looks::Look;
use lumenply_doc::Lut3D;

/// Each built-in look's table, baked once and shared, so choosing a look
/// again reuses the same table.
fn look_table(look: Look) -> Arc<Lut3D> {
    static TABLES: OnceLock<Vec<Arc<Lut3D>>> = OnceLock::new();
    let all = TABLES.get_or_init(|| Look::ALL.iter().map(|l| Arc::new(l.table())).collect());
    let i = Look::ALL.iter().position(|l| *l == look).unwrap_or(0);
    all[i].clone()
}

/// The lookup adjustment for a built-in look.
pub(crate) fn look_adjustment(look: Look) -> Adjustment {
    Adjustment::ColorLookup {
        lut: look_table(look),
        name: look.name().into(),
    }
}

/// The built-in look a lookup shows, judged by its names (cheap enough to
/// ask every frame; a reloaded project has its own copy of the table).
fn current_look(lut: &Lut3D, name: &str) -> Option<Look> {
    Look::ALL
        .into_iter()
        .find(|l| name == l.name() && lut.title == l.name())
}

/// One line describing the table: its name and shape.
fn describe(lut: &Lut3D, name: &str) -> String {
    if name.is_empty() && lut.is_identity() {
        return "No table chosen: pick a look or load a .cube or .3dl file.".into();
    }
    let shape = match &lut.shaper {
        Some(s) if lut.size == 2 => format!("1D table, {} entries", s.data.len()),
        Some(s) => format!("{}-entry 1D + {n}×{n}×{n} table", s.data.len(), n = lut.size),
        None => format!("{n}×{n}×{n} table", n = lut.size),
    };
    let name = if name.is_empty() { "Untitled" } else { name };
    format!("{name} · {shape}")
}

impl App {
    /// Properties for a Color Lookup layer; edits `lut` and `name` in
    /// place (the caller turns a change into a `SetAdjustment`). Returns
    /// true when an edit finished.
    pub(crate) fn color_lookup_ui(
        &mut self,
        ui: &mut egui::Ui,
        id: LayerId,
        lut: &mut Arc<Lut3D>,
        name: &mut String,
    ) -> bool {
        let mut finished = false;
        let current = current_look(lut, name);
        let none = name.is_empty() && lut.is_identity();
        let shown = match current {
            Some(l) => l.name(),
            None if none => "None",
            None => "Custom",
        };
        ui.horizontal(|ui| {
            row_label(ui, "Look", LABEL_W);
            ui.spacing_mut().combo_width = ui.available_width();
            let r = egui::ComboBox::from_id_salt(("lut-look", id))
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    popup_style(ui);
                    if ui.selectable_label(none, "None").clicked() {
                        *lut = Arc::new(Lut3D::identity(2));
                        name.clear();
                        finished = true;
                    }
                    for look in Look::ALL {
                        if ui.selectable_label(current == Some(look), look.name()).clicked() {
                            *lut = look_table(look);
                            *name = look.name().into();
                            finished = true;
                        }
                    }
                });
            a11y_name(&r.response, "Look");
        });
        ui.horizontal(|ui| {
            row_label(ui, "3D LUT file", LABEL_W);
            let b = ui
                .button("Load 3D LUT…")
                .on_hover_text("Load a .cube or .3dl file; the table is saved inside the document");
            if b.clicked() {
                if let Some((l, n)) = self.pick_lut_file() {
                    *lut = l;
                    *name = n;
                    finished = true;
                }
            }
        });
        ui.label(RichText::new(describe(lut, name)).small().color(MUTED));
        finished
    }

    /// Ask for a `.cube` / `.3dl` file and read it; failures go to the
    /// status bar.
    pub(crate) fn pick_lut_file(&mut self) -> Option<(Arc<Lut3D>, String)> {
        let p = self
            .file_dialog()
            .set_title("Load 3D LUT")
            .add_filter("3D LUT", &["cube", "CUBE", "3dl", "3DL"])
            .pick_file()?;
        self.read_lut_file(&p)
    }

    pub(crate) fn read_lut_file(&mut self, p: &Path) -> Option<(Arc<Lut3D>, String)> {
        let file = p
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        match lumenply_io::lut_files::load_lut(p) {
            Ok(l) => {
                let name = p
                    .file_stem()
                    .map_or(file.clone(), |n| n.to_string_lossy().into_owned());
                self.status = format!("Loaded {file}: {}", describe(&l, &name));
                Some((Arc::new(l), name))
            }
            Err(e) => {
                self.status = format!("Could not load {file}: {e}");
                None
            }
        }
    }

    /// Put a table on the active Color Lookup layer, or add a new one
    /// above the active layer.
    pub(crate) fn apply_lookup(&mut self, adjustment: Adjustment) {
        let target = self.active.filter(|id| {
            matches!(
                self.editor.doc().layer(*id).map(|l| &l.content),
                Some(LayerContent::Adjustment(Adjustment::ColorLookup { .. }))
            )
        });
        match target {
            Some(layer) => self.run(&SetAdjustment { layer, adjustment }),
            None => self.add_adjustment(adjustment),
        }
    }

    /// The `load-lut` action: pick a LUT file and apply it.
    pub(crate) fn load_lut_action(&mut self) {
        if let Some((lut, name)) = self.pick_lut_file() {
            self.apply_lookup(Adjustment::ColorLookup { lut, name });
        }
    }

    /// Whether any visible adjustment layer would go into an exported table.
    pub(crate) fn has_visible_adjustments(&self) -> bool {
        let mut any = false;
        self.editor.doc().for_each_layer(|l| {
            any |= l.visible && matches!(l.content, LayerContent::Adjustment(_));
        });
        any
    }

    /// File ▸ Export ▸ Color Lookup Table: the visible adjustment layers
    /// baked into a 33³ `.cube` (Photoshop's Export Color Lookup Tables).
    pub(crate) fn pick_export_lut(&mut self) {
        if let Some(p) = self.pick_save_path("Export Color Lookup Table", "Cube LUT", &["cube"]) {
            self.export_lut_to(Path::new(&p));
        }
    }

    pub(crate) fn export_lut_to(&mut self, p: &Path) {
        let title = p
            .file_stem()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let (lut, count) = lumenply_render::lut_bake::bake_adjustments(self.editor.doc(), 33, &title);
        if count == 0 {
            self.status = "No visible adjustment layers to export".into();
            return;
        }
        let what = if count == 1 { "layer" } else { "layers" };
        self.status = match std::fs::write(p, lumenply_io::lut_files::write_cube(&lut)) {
            Ok(()) => format!("Exported {} ({count} adjustment {what}, 33×33×33)", p.display()),
            Err(e) => format!("Could not export: {e}"),
        };
    }

    /// Screenshot tokens (`lut:...`): `lut:add=<look>` adds a Color Lookup
    /// layer with a built-in look (`warm`, `cool`, `teal-orange`,
    /// `bleach-bypass`, `faded-film`, `mono-contrast`, `crisp`) above the
    /// active layer; `lut:look=<look|none>` sets the active lookup's look;
    /// `lut:load=<path>` applies a `.cube` / `.3dl` file like `load-lut`;
    /// `lut:export=<path>` bakes the adjustments to a `.cube` there.
    pub(crate) fn debug_lut(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("lut:") else {
            return false;
        };
        let (key, arg) = rest.split_once('=').unwrap_or((rest, ""));
        match key {
            "add" => {
                if let Some(look) = Look::from_id(arg) {
                    self.add_adjustment(look_adjustment(look));
                }
            }
            "look" => match Look::from_id(arg) {
                Some(look) => self.apply_lookup(look_adjustment(look)),
                None => self.apply_lookup(Adjustment::color_lookup_default()),
            },
            "load" => {
                if let Some((lut, name)) = self.read_lut_file(Path::new(arg)) {
                    self.apply_lookup(Adjustment::ColorLookup { lut, name });
                }
            }
            "export" => self.export_lut_to(Path::new(arg)),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_are_baked_once_and_named() {
        let a = look_table(Look::TealOrange);
        assert!(Arc::ptr_eq(&a, &look_table(Look::TealOrange)));
        assert_eq!(current_look(&a, "Teal & Orange"), Some(Look::TealOrange));
        assert_eq!(current_look(&a, "Renamed"), None);
        assert_eq!(describe(&a, "Teal & Orange"), "Teal & Orange · 33×33×33 table");
        let id = Lut3D::identity(2);
        assert!(describe(&id, "").starts_with("No table chosen"));
    }

    #[test]
    fn exporting_bakes_the_adjustments_and_reloads_as_the_same_look() {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(crate::blank(16, 16), None);
        let dir = std::env::temp_dir().join(format!("lumenply-lut-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Graded.cube");
        assert_eq!(
            app.action_block("export-lut"),
            Some("Add an adjustment layer first")
        );
        app.export_lut_to(&path);
        assert_eq!(app.status, "No visible adjustment layers to export");
        assert!(!path.exists());
        // Invert, exported, then loaded back: a 33³ table inverting gamma
        // values — (0.25, 0.5, 1) becomes (0.75, 0.5, 0).
        app.add_adjustment(Adjustment::Invert);
        assert_eq!(app.action_block("export-lut"), None);
        app.export_lut_to(&path);
        assert!(
            app.status.contains("1 adjustment layer, 33×33×33"),
            "{}",
            app.status
        );
        let (lut, name) = app.read_lut_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            (name.as_str(), lut.title.as_str(), lut.size),
            ("Graded", "Graded", 33)
        );
        let o = lut.apply([0.25, 0.5, 1.0]);
        assert!(
            (o[0] - 0.75).abs() < 1e-3 && (o[1] - 0.5).abs() < 1e-3 && o[2].abs() < 1e-3,
            "{o:?}"
        );
    }
}
