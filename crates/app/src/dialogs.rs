use std::path::Path;

use super::*;

pub(crate) enum Dialog {
    ExportJpeg(String, u8),
    New(u32, u32),
    Filter(Filter),
    CanvasSize(u32, u32, (f32, f32)),
    ImageSize(u32, u32, bool),
    /// The window close was intercepted because of unsaved changes.
    ConfirmClose,
    /// Closing one document tab (by display index) with unsaved changes.
    ConfirmCloseTab(usize),
    /// An autosave backup from a previous session was found at startup.
    Recover,
    Preferences(session::Prefs, Option<String>),
    /// Colour-range selection: (tolerance %, whether a preview ran).
    ColorRange(f32, bool),
    About,
}

/// Extensions the open dialogs offer, by kind. The first of each list is
/// the one a save dialog appends when the typed name lacks it.
pub(crate) const PROJECT_EXT: &[&str] = &["lumen", "nge"];
pub(crate) const PSD_EXT: &[&str] = &["psd", "psb"];
pub(crate) const ORA_EXT: &[&str] = &["ora"];
pub(crate) const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "webp", "exr"];

/// Where a file dialog starts: the live document's folder, else the folder
/// of the most recently used file that still exists (None = the OS default).
pub(crate) fn dialog_start_dir(doc_path: Option<&Path>, recent: &[String]) -> Option<PathBuf> {
    let usable = |d: &Path| !d.as_os_str().is_empty() && d.is_dir();
    if let Some(dir) = doc_path.and_then(Path::parent).filter(|d| usable(d)) {
        return Some(dir.to_path_buf());
    }
    recent
        .iter()
        .filter_map(|r| Path::new(r).parent())
        .find(|d| usable(d))
        .map(Path::to_path_buf)
}

/// The suggested file name (without extension) for a save or export: the
/// document's own name, else its tab label (an imported "photo.jpg" offers
/// "photo"), else "Untitled".
pub(crate) fn default_stem(doc_path: Option<&Path>, label: &str) -> String {
    let from = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().trim().to_string())
            .filter(|s| !s.is_empty())
    };
    doc_path
        .and_then(from)
        .or_else(|| from(Path::new(label)))
        .unwrap_or_else(|| "Untitled".into())
}

/// Make sure a chosen save path carries one of `allowed` (case-insensitive);
/// otherwise append the first. "shot.v2" becomes "shot.v2.png", never
/// "shot.png", so a dotted name is not silently truncated.
pub(crate) fn enforce_extension(p: PathBuf, allowed: &[&str]) -> PathBuf {
    let ok = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| allowed.iter().any(|a| a.eq_ignore_ascii_case(e)));
    if ok || allowed.is_empty() {
        return p;
    }
    let mut s = p.into_os_string();
    s.push(".");
    s.push(allowed[0]);
    PathBuf::from(s)
}

impl App {
    fn file_dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = dialog_start_dir(self.path.as_deref(), &self.recent) {
            d = d.set_directory(dir);
        }
        d
    }

    pub(crate) fn pick_open(&mut self) {
        let all: Vec<&str> = [PROJECT_EXT, PSD_EXT, ORA_EXT, IMAGE_EXT].concat();
        if let Some(p) = self
            .file_dialog()
            .set_title("Open")
            .add_filter("All supported files", &all)
            .add_filter("Lumenply projects", PROJECT_EXT)
            .add_filter("Photoshop documents", PSD_EXT)
            .add_filter("OpenRaster", ORA_EXT)
            .add_filter("Images", IMAGE_EXT)
            .pick_file()
        {
            self.open_path(&p.to_string_lossy());
        }
    }

    pub(crate) fn pick_open_image(&mut self) {
        if let Some(p) = self
            .file_dialog()
            .set_title("Open image")
            .add_filter("Images", IMAGE_EXT)
            .pick_file()
        {
            self.open_image(&p.to_string_lossy());
        }
    }

    pub(crate) fn pick_place(&mut self) {
        if let Some(p) = self
            .file_dialog()
            .set_title(if self.no_doc {
                "Open image"
            } else {
                "Place image as layer"
            })
            .add_filter("Images", IMAGE_EXT)
            .pick_file()
        {
            self.place_image(&p.to_string_lossy());
        }
    }

    /// Native save panel for one format; returns the chosen path with its
    /// extension enforced. `exts[0]` is the default.
    fn pick_save_path(&self, title: &str, what: &str, exts: &[&str]) -> Option<String> {
        if self.no_doc {
            return None; // nothing to save
        }
        let stem = default_stem(self.path.as_deref(), &self.untitled);
        self.file_dialog()
            .set_title(title)
            .add_filter(what, exts)
            .set_file_name(format!("{stem}.{}", exts[0]))
            .save_file()
            .map(|p| enforce_extension(p, exts).to_string_lossy().into_owned())
    }

    pub(crate) fn pick_save(&mut self) {
        if let Some(p) = self.pick_save_path("Save project", "Lumenply project", PROJECT_EXT) {
            self.save_path(&p);
        }
    }

    pub(crate) fn pick_export_png(&mut self) {
        if let Some(p) = self.pick_save_path("Export PNG", "PNG image", &["png"]) {
            self.export_png(&p);
        }
    }

    pub(crate) fn pick_export_psd(&mut self) {
        if let Some(p) = self.pick_save_path("Export Photoshop PSD", "Photoshop document", &["psd"]) {
            self.export_psd(&p);
        }
    }

    pub(crate) fn pick_export_psd16(&mut self) {
        if let Some(p) = self.pick_save_path(
            "Export Photoshop PSD (16-bit)",
            "Photoshop document (16-bit)",
            &["psd"],
        ) {
            match lumenply_io::psd::save_16(&p, self.editor.doc()) {
                Ok(rep) => {
                    self.status = if rep.warnings.is_empty() {
                        format!("Exported {p} (16-bit)")
                    } else {
                        format!(
                            "Exported {p} (16-bit); not carried over: {}",
                            rep.warnings.join("; ")
                        )
                    };
                }
                Err(e) => self.status = format!("Could not export: {e}"),
            }
        }
    }

    pub(crate) fn pick_export_exr(&mut self) {
        if let Some(p) = self.pick_save_path("Export OpenEXR", "OpenEXR (linear float)", &["exr"]) {
            let flat = lumenply_render::composite_raster(self.editor.doc());
            match lumenply_io::save_exr(&p, &flat) {
                Ok(()) => self.status = format!("Exported {p}"),
                Err(e) => self.status = format!("Could not export: {e}"),
            }
        }
    }

    pub(crate) fn pick_export_16bit(&mut self) {
        if self.no_doc {
            return;
        }
        let stem = default_stem(self.path.as_deref(), &self.untitled);
        let picked = self
            .file_dialog()
            .set_title("Export 16-bit PNG or TIFF")
            .add_filter("16-bit PNG", &["png"])
            .add_filter("16-bit TIFF", &["tif", "tiff"])
            .set_file_name(format!("{stem}.png"))
            .save_file();
        if let Some(p) = picked {
            // Either container is fine; anything else gets ".png".
            let p = enforce_extension(p, &["png", "tif", "tiff"]);
            let flat = lumenply_render::composite_raster(self.editor.doc());
            match lumenply_io::save_16bit(&p, &flat) {
                Ok(()) => self.status = format!("Exported {}", p.display()),
                Err(e) => self.status = format!("Could not export: {e}"),
            }
        }
    }

    pub(crate) fn pick_export_ora(&mut self) {
        if let Some(p) = self.pick_save_path("Export OpenRaster", "OpenRaster", ORA_EXT) {
            self.export_ora(&p);
        }
    }

    pub(crate) fn export_ora(&mut self, path: &str) {
        match lumenply_io::ora::save(path, self.editor.doc()) {
            Ok(rep) => {
                self.status = if rep.warnings.is_empty() {
                    format!("Exported {path}")
                } else {
                    format!("Exported {path} ({})", rep.warnings.join("; "))
                };
            }
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    pub(crate) fn pick_export_jpeg(&mut self) {
        if let Some(p) = self.pick_save_path("Export JPEG", "JPEG image", &["jpg", "jpeg"]) {
            self.dialog = Some(Dialog::ExportJpeg(p, 90));
        }
    }
}

impl App {
    // ---- files -----------------------------------------------------------------

    pub(crate) fn open_path(&mut self, path: &str) {
        // A file already open in a tab comes forward instead of reopening.
        if self.focus_tab_with_path(path) {
            self.status = format!("Switched to {path}");
            return;
        }
        if is_ora_path(path) {
            match lumenply_io::ora::load(path) {
                Ok(rep) => {
                    let n = rep.warnings.len();
                    self.open_in_new_tab(Editor::new(rep.value), None);
                    // Imports keep their file name on the tab until saved
                    // as a project.
                    self.untitled = file_name(path);
                    self.recent = session::push_recent(path);
                    self.status = if n == 0 {
                        format!("Imported {path}")
                    } else {
                        format!("Imported {path} ({})", rep.warnings.join("; "))
                    };
                }
                Err(e) => self.status = format!("Could not import {path}: {e}"),
            }
            return;
        }
        if is_psd_path(path) {
            match lumenply_io::psd::load(path) {
                Ok(rep) => {
                    let n = rep.warnings.len();
                    self.open_in_new_tab(Editor::new(rep.value), None);
                    self.untitled = file_name(path);
                    self.recent = session::push_recent(path);
                    self.status = if n == 0 {
                        format!("Imported {path}")
                    } else {
                        format!("Imported {path} ({n} items skipped: {})", rep.warnings.join("; "))
                    };
                }
                Err(e) => self.status = format!("Could not import {path}: {e}"),
            }
            return;
        }
        if is_image_path(path) {
            self.open_image(path);
            return;
        }
        match project::load(path) {
            Ok(doc) => {
                self.open_in_new_tab(Editor::new(doc), Some(PathBuf::from(path)));
                self.recent = session::push_recent(path);
                self.status = format!("Opened {path}");
                self.note_missing_fonts();
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    pub(crate) fn export_psd(&mut self, path: &str) {
        match lumenply_io::psd::save(path, self.editor.doc()) {
            Ok(rep) => {
                self.status = if rep.warnings.is_empty() {
                    format!("Exported {path}")
                } else {
                    format!("Exported {path}; not carried over: {}", rep.warnings.join("; "))
                };
            }
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    pub(crate) fn open_image(&mut self, path: &str) {
        match lumenply_io::load(path) {
            Ok(raster) => {
                let (w, h) = (raster.width, raster.height);
                let mut doc = Document::new(w, h);
                // EXR is linear float: open in float mode so HDR values
                // survive the import (and every edit after it).
                doc.float_mode = path.to_ascii_lowercase().ends_with(".exr");
                let mut ed = Editor::new(doc);
                let _ = ed.execute(&AddPixelLayer::from_raster("Background", raster, 0, 0));
                // A fresh editor: the opened image is the starting point,
                // not an undoable "Add layer" step.
                self.open_in_new_tab(Editor::new(ed.doc().clone()), None);
                self.untitled = file_name(path);
                self.recent = session::push_recent(path);
                self.status = format!("Opened {path} ({w}×{h})");
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    pub(crate) fn place_image(&mut self, path: &str) {
        if self.no_doc {
            // Nothing to place into: open the image as its own document.
            self.open_image(path);
            return;
        }
        match lumenply_io::load(path) {
            Ok(raster) => {
                let doc = self.editor.doc();
                let x = (doc.width as i32 - raster.width as i32) / 2;
                let y = (doc.height as i32 - raster.height as i32) / 2;
                self.run(&AddPixelLayer::from_raster(file_name(path), raster, x, y));
                self.select_top();
                self.status = format!("Placed {path}");
            }
            Err(e) => self.status = format!("Could not place {path}: {e}"),
        }
    }

    pub(crate) fn save_path(&mut self, path: &str) {
        match project::save(path, self.editor.doc()) {
            Ok(()) => {
                self.path = Some(PathBuf::from(path));
                self.saved_rev = self.editor.history().len();
                self.recent = session::push_recent(path);
                session::remove_autosave();
                self.status = format!("Saved {path}");
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
    }

    pub(crate) fn export_png(&mut self, path: &str) {
        let flat = lumenply_render::composite_raster(self.editor.doc());
        match lumenply_io::save_png(path, &flat) {
            Ok(()) => self.status = format!("Exported {path}"),
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    pub(crate) fn export_jpeg(&mut self, path: &str, quality: u8) {
        let flat = lumenply_render::composite_raster(self.editor.doc());
        match lumenply_io::save_jpeg(path, &flat, quality) {
            Ok(()) => self.status = format!("Exported {path} (quality {quality})"),
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    // ---- dialogs ---------------------------------------------------------------------

    pub(crate) fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dialog.take() else { return };
        let title = match &d {
            Dialog::ExportJpeg(..) => "Export JPEG",
            Dialog::New(..) => "New document",
            Dialog::ConfirmClose => "Unsaved changes",
            Dialog::ConfirmCloseTab(_) => "Close document",
            Dialog::Recover => "Recover autosaved document",
            Dialog::Preferences(..) => "Preferences",
            Dialog::ColorRange(..) => "Colour range",
            Dialog::About => "About",
            Dialog::Filter(f) => f.name(),
            Dialog::CanvasSize(..) => "Canvas size",
            Dialog::ImageSize(..) => "Image size",
        };
        let mut keep = true;
        let mut confirmed = false;
        let mut filter_changed = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                match &mut d {
                    Dialog::About => {
                        ui.vertical_centered(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
                            ui.painter().rect_filled(r, 22.0, GROUND);
                            brand::paint_mark(ui.painter(), r.shrink(14.0), TEXT, ACCENT, GROUND);
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Lumenply")
                                    .family(egui::FontFamily::Name("semibold".into()))
                                    .size(26.0)
                                    .color(TEXT),
                            );
                            ui.label(
                                RichText::new(format!("Public beta {}", env!("CARGO_PKG_VERSION")))
                                    .monospace()
                                    .color(MUTED),
                            );
                            ui.add_space(4.0);
                            ui.label(RichText::new("Free photo editor — GPL-3.0").color(MUTED));
                            ui.add_space(6.0);
                            if ui.button("Close").clicked() {
                                keep = false;
                            }
                        });
                    }
                    Dialog::ColorRange(tol, previewed) => {
                        ui.label("Selects everything close to the brush colour.");
                        let changed = ui
                            .add(egui::Slider::new(tol, 1.0..=100.0).suffix("%").text("Fuzziness"))
                            .changed();
                        // The colour being matched, editable (and sampleable) in place.
                        let recolored = ui
                            .horizontal(|ui| {
                                ui.label("Brush colour");
                                crate::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb).changed()
                            })
                            .inner;
                        if changed || recolored || !*previewed {
                            *previewed = true;
                            self.run_coalescing(
                                &SelectColorRange {
                                    color: self.brush_rgb.map(lumenply_io::srgb_to_linear_f),
                                    tolerance: *tol / 100.0,
                                },
                                "color-range",
                            );
                        }
                    }
                    Dialog::Preferences(p, capturing) => {
                        let mut steps = p.undo_steps as f32;
                        if ui
                            .add(
                                egui::Slider::new(&mut steps, 1.0..=1000.0)
                                    .integer()
                                    .text("Undo steps"),
                            )
                            .changed()
                        {
                            p.undo_steps = steps as usize;
                        }
                        let mut mb = p.undo_memory_mb as f32;
                        if ui
                            .add(
                                egui::Slider::new(&mut mb, 64.0..=8192.0)
                                    .logarithmic(true)
                                    .integer()
                                    .suffix(" MB")
                                    .text("Undo memory"),
                            )
                            .changed()
                        {
                            p.undo_memory_mb = mb as usize;
                        }
                        let mut secs = p.autosave_secs as f32;
                        if ui
                            .add(
                                egui::Slider::new(&mut secs, 15.0..=600.0)
                                    .integer()
                                    .suffix(" s")
                                    .text("Autosave every"),
                            )
                            .changed()
                        {
                            p.autosave_secs = secs as u64;
                        }
                        ui.horizontal(|ui| {
                            ui.label("Canvas surround");
                            for (name, c) in [
                                ("Graphite", [0x14u8, 0x16, 0x19]),
                                ("Black", [0x00, 0x00, 0x00]),
                                ("Grey", [0x80, 0x80, 0x80]),
                                ("Light", [0xD8, 0xD8, 0xD8]),
                            ] {
                                let on = p.canvas_bg == c;
                                let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                                ui.painter().rect_filled(
                                    r.shrink(2.0),
                                    3.0,
                                    Color32::from_rgb(c[0], c[1], c[2]),
                                );
                                if on {
                                    ui.painter().rect_stroke(r, 4.0, Stroke::new(2.0, ACCENT));
                                }
                                if resp.on_hover_text(name).clicked() {
                                    p.canvas_bg = c;
                                }
                            }
                            // Any other colour, through the shared picker.
                            let mut custom = p.canvas_bg.map(|b| b as f32 / 255.0);
                            if crate::color_picker::color_edit_button_rgb(ui, &mut custom).changed() {
                                p.canvas_bg = crate::color_picker::to_u8(custom);
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("SHORTCUTS").small().strong().color(MUTED));
                        // Click a binding, press the new keys; Esc cancels.
                        if let Some(active) = capturing.clone() {
                            let got = ui.input(|i| {
                                i.events.iter().find_map(|e| match e {
                                    egui::Event::Key {
                                        key,
                                        pressed: true,
                                        modifiers,
                                        ..
                                    } => Some((*key, *modifiers)),
                                    _ => None,
                                })
                            });
                            if let Some((key, m)) = got {
                                if key == Key::Escape {
                                    *capturing = None;
                                } else if !matches!(key, Key::Tab | Key::Enter | Key::Space) {
                                    p.shortcuts.insert(
                                        active,
                                        session::Chord {
                                            cmd: m.command,
                                            shift: m.shift,
                                            key: key.name().to_string(),
                                        },
                                    );
                                    *capturing = None;
                                }
                            }
                        }
                        for (id, label, ..) in session::SHORTCUTS {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [120.0, 18.0],
                                    egui::Label::new(RichText::new(*label).color(MUTED)),
                                );
                                let text = if capturing.as_deref() == Some(*id) {
                                    "press keys...".to_string()
                                } else {
                                    session::chord_label(p, id)
                                };
                                let highlight = capturing.as_deref() == Some(*id);
                                let btn = egui::Button::new(RichText::new(text).monospace())
                                    .min_size(egui::vec2(110.0, 20.0))
                                    .fill(if highlight { ACCENT_TINT } else { GROUND })
                                    .stroke(Stroke::new(1.0, if highlight { ACCENT } else { LINE }));
                                if ui.add(btn).clicked() {
                                    *capturing = Some(id.to_string());
                                }
                                if p.shortcuts.contains_key(*id) && ui.small_button("reset").clicked() {
                                    p.shortcuts.remove(*id);
                                }
                            });
                        }
                    }
                    Dialog::Recover => {
                        ui.label("The previous session left an autosaved backup,");
                        ui.label("probably after a crash.");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Recover").clicked() {
                                self.recover_autosave();
                                keep = false;
                            }
                            if ui.button("Discard backup").clicked() {
                                session::remove_autosave();
                                keep = false;
                            }
                        });
                    }
                    Dialog::ConfirmClose => {
                        ui.label("The document has unsaved changes.");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save and quit").clicked() {
                                match self.path.clone() {
                                    Some(p) => self.save_path(&p.to_string_lossy()),
                                    None => self.pick_save(),
                                }
                                if self.editor.history().len() == self.saved_rev {
                                    self.allow_close = true;
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                                keep = false;
                            }
                            if ui.button("Quit without saving").clicked() {
                                self.allow_close = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                keep = false;
                            }
                            if ui.button("Cancel").clicked() {
                                keep = false;
                            }
                        });
                    }
                    Dialog::ConfirmCloseTab(i) => {
                        let i = *i;
                        ui.label("This document has unsaved changes.");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save and close").clicked() {
                                match self.path.clone() {
                                    Some(p) => self.save_path(&p.to_string_lossy()),
                                    None => self.pick_save(),
                                }
                                if self.editor.history().len() == self.saved_rev {
                                    self.force_close_tab(i);
                                }
                                keep = false;
                            }
                            if ui.button("Close without saving").clicked() {
                                self.force_close_tab(i);
                                keep = false;
                            }
                            if ui.button("Cancel").clicked() {
                                keep = false;
                            }
                        });
                    }
                    Dialog::ExportJpeg(p, q) => {
                        ui.label(RichText::new(file_name(p)).monospace());
                        let mut qf = *q as f32;
                        ui.add(egui::Slider::new(&mut qf, 1.0..=100.0).integer().text("Quality"));
                        *q = qf.round() as u8;
                        ui.label(RichText::new("Transparent areas are flattened onto white.").weak());
                    }
                    Dialog::New(w, h) => {
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            ui.add(egui::DragValue::new(w).range(1..=16384).suffix(" px"));
                            ui.label("Height");
                            ui.add(egui::DragValue::new(h).range(1..=16384).suffix(" px"));
                        });
                    }
                    Dialog::Filter(f) => match f {
                        Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
                            filter_changed |= ui
                                .add(
                                    egui::Slider::new(radius, 0.5..=60.0)
                                        .logarithmic(true)
                                        .suffix(" px")
                                        .text("Radius"),
                                )
                                .changed();
                        }
                        Filter::Sharpen { amount, radius } => {
                            filter_changed |= ui
                                .add(egui::Slider::new(amount, 0.0..=5.0).text("Amount"))
                                .changed();
                            filter_changed |= ui
                                .add(egui::Slider::new(radius, 0.5..=20.0).suffix(" px").text("Radius"))
                                .changed();
                        }
                        Filter::Noise { amount } => {
                            filter_changed |= ui
                                .add(egui::Slider::new(amount, 0.0..=1.0).text("Amount"))
                                .changed();
                        }
                        Filter::MotionBlur { angle, distance } => {
                            filter_changed |= ui
                                .add(egui::Slider::new(angle, -180.0..=180.0).suffix("°").text("Angle"))
                                .changed();
                            filter_changed |= ui
                                .add(
                                    egui::Slider::new(distance, 1.0..=200.0)
                                        .suffix(" px")
                                        .text("Distance"),
                                )
                                .changed();
                        }
                        Filter::Median { radius } => {
                            filter_changed |= ui
                                .add(
                                    egui::Slider::new(radius, 1.0..=8.0)
                                        .integer()
                                        .suffix(" px")
                                        .text("Radius"),
                                )
                                .changed();
                        }
                        Filter::HighPass { radius } => {
                            filter_changed |= ui
                                .add(
                                    egui::Slider::new(radius, 0.5..=60.0)
                                        .logarithmic(true)
                                        .suffix(" px")
                                        .text("Radius"),
                                )
                                .changed();
                        }
                    },
                    Dialog::CanvasSize(w, h, anchor) => {
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            ui.add(egui::DragValue::new(w).range(1..=16384).suffix(" px"));
                            ui.label("Height");
                            ui.add(egui::DragValue::new(h).range(1..=16384).suffix(" px"));
                        });
                        ui.label("Anchor");
                        for row in 0..3 {
                            ui.horizontal(|ui| {
                                for col in 0..3 {
                                    let a = (col as f32 * 0.5, row as f32 * 0.5);
                                    let on = (anchor.0 - a.0).abs() < 1e-3 && (anchor.1 - a.1).abs() < 1e-3;
                                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                                    let fill = if on { ACCENT } else { RAISED };
                                    ui.painter().rect_filled(r.shrink(3.0), 3.0, fill);
                                    if on {
                                        ui.painter().circle_filled(r.center(), 4.0, Color32::WHITE);
                                    }
                                    if resp.clicked() {
                                        *anchor = a;
                                    }
                                }
                            });
                        }
                    }
                    Dialog::ImageSize(w, h, lock) => {
                        let (ow, oh) = (self.editor.doc().width as f32, self.editor.doc().height as f32);
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            let rw = ui.add(egui::DragValue::new(w).range(1..=16384).suffix(" px"));
                            ui.label("Height");
                            let rh = ui.add(egui::DragValue::new(h).range(1..=16384).suffix(" px"));
                            if *lock {
                                if rw.changed() {
                                    *h = ((*w as f32) * oh / ow).round().max(1.0) as u32;
                                } else if rh.changed() {
                                    *w = ((*h as f32) * ow / oh).round().max(1.0) as u32;
                                }
                            }
                        });
                        ui.checkbox(lock, "Keep aspect ratio");
                        ui.label(RichText::new("Resamples every layer bilinearly.").weak());
                    }
                }
                if !matches!(
                    d,
                    Dialog::ConfirmClose | Dialog::ConfirmCloseTab(_) | Dialog::Recover | Dialog::About
                ) {
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            confirmed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
            });

        if let Dialog::Filter(f) = &d {
            if filter_changed || !self.filter_previewed {
                if let Some(layer) = self.active {
                    let mut preview = self.editor.doc().clone();
                    if (ApplyFilter {
                        layer,
                        filter: f.clone(),
                    })
                    .apply(&mut preview)
                    .is_ok()
                    {
                        self.preview(ctx, &preview, None);
                    }
                }
                self.filter_previewed = true;
            }
        }

        if confirmed {
            match &d {
                Dialog::ConfirmClose | Dialog::ConfirmCloseTab(_) | Dialog::Recover | Dialog::About => {}
                Dialog::ColorRange(..) => {}
                Dialog::Preferences(p, _) => {
                    self.prefs = p.clone();
                    self.prefs.apply(&mut self.editor);
                    self.prefs.save();
                    self.status = "Preferences saved".into();
                }
                Dialog::ExportJpeg(p, q) => self.export_jpeg(p, *q),
                Dialog::New(w, h) => self.open_in_new_tab(blank(*w, *h), None),
                Dialog::Filter(f) => {
                    if let Some(layer) = self.active {
                        self.run(&ApplyFilter {
                            layer,
                            filter: f.clone(),
                        });
                    }
                }
                Dialog::CanvasSize(w, h, anchor) => {
                    self.run(&ResizeCanvas {
                        width: *w,
                        height: *h,
                        anchor: *anchor,
                    });
                    self.view_cmd = Some(ViewCmd::Fit);
                }
                Dialog::ImageSize(w, h, _) => {
                    self.run(&ResizeImage {
                        width: *w,
                        height: *h,
                    });
                    self.view_cmd = Some(ViewCmd::Fit);
                }
            }
            keep = false;
        }
        if keep {
            self.dialog = Some(d);
        } else {
            if let Dialog::ColorRange(_, true) = d {
                self.editor.end_coalescing();
                if !confirmed {
                    // Cancel: the previewed selection was one coalesced step.
                    self.undo();
                }
            }
            if matches!(d, Dialog::Filter(_)) {
                // Drop the preview whether confirmed or cancelled.
                self.mark(None);
            }
            self.filter_previewed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_paths_get_the_right_extension() {
        let e = |p: &str, allowed: &[&str]| enforce_extension(PathBuf::from(p), allowed);
        assert_eq!(e("/x/photo", &["png"]), PathBuf::from("/x/photo.png"));
        assert_eq!(e("/x/photo.png", &["png"]), PathBuf::from("/x/photo.png"));
        assert_eq!(e("/x/photo.PNG", &["png"]), PathBuf::from("/x/photo.PNG"));
        // A dotted name keeps its dots instead of losing "v2".
        assert_eq!(e("/x/shot.v2", &["png"]), PathBuf::from("/x/shot.v2.png"));
        // Either accepted spelling stays; the first is the default.
        assert_eq!(e("/x/a.jpeg", &["jpg", "jpeg"]), PathBuf::from("/x/a.jpeg"));
        assert_eq!(e("/x/a", &["jpg", "jpeg"]), PathBuf::from("/x/a.jpg"));
        assert_eq!(e("/x/a.tif", &["png", "tif", "tiff"]), PathBuf::from("/x/a.tif"));
        assert_eq!(e("/x/old.nge", PROJECT_EXT), PathBuf::from("/x/old.nge"));
        assert_eq!(e("/x/new", PROJECT_EXT), PathBuf::from("/x/new.lumen"));
    }

    #[test]
    fn suggested_names_follow_the_document() {
        let p = PathBuf::from("/a/b/summit.lumen");
        assert_eq!(default_stem(Some(&p), "ignored"), "summit");
        // Imports carry their file name on the tab.
        assert_eq!(default_stem(None, "photo.jpg"), "photo");
        assert_eq!(default_stem(None, "Untitled-3"), "Untitled-3");
        assert_eq!(default_stem(None, "Aoraki demo"), "Aoraki demo");
        assert_eq!(default_stem(None, ""), "Untitled");
    }

    #[test]
    fn dialogs_start_in_a_folder_that_exists() {
        let base = std::env::temp_dir().join("lumenply-dialog-dir-test");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let doc = a.join("doc.lumen");
        let gone = base.join("deleted").join("x.png").to_string_lossy().into_owned();
        let recent = vec![gone, b.join("pic.png").to_string_lossy().into_owned()];
        assert_eq!(dialog_start_dir(Some(&doc), &recent), Some(a.clone()));
        // No document path: the newest recent file whose folder still exists.
        assert_eq!(dialog_start_dir(None, &recent), Some(b.clone()));
        assert_eq!(dialog_start_dir(None, &[]), None);
        // A bare file name has no usable folder.
        assert_eq!(dialog_start_dir(Some(Path::new("loose.lumen")), &[]), None);
    }

    #[test]
    fn every_dialog_image_type_is_treated_as_an_image() {
        for ext in IMAGE_EXT {
            assert!(is_image_path(&format!("x.{ext}")), "{ext}");
        }
        for ext in PSD_EXT {
            assert!(is_psd_path(&format!("x.{ext}")), "{ext}");
        }
        assert!(is_ora_path("x.ora"));
    }
}
