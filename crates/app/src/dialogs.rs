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
    /// Select > Modify: the reshaping, and whether its preview step is on
    /// the history (replaced on every change, undone on cancel).
    SelectEdge(EdgeOp, bool),
    /// Edit > Fill: (content-aware rather than the brush colour, sampling
    /// margin in px, busy phase: 0 idle, 1 showing "Filling", 2 running).
    Fill(bool, f32, u8),
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
        // The frame after "Filling" was shown: do the (blocking) work.
        if let Dialog::Fill(true, margin, 2) = d {
            if let Some(layer) = self.active {
                let t = std::time::Instant::now();
                self.run(&ContentAwareFill {
                    layer,
                    margin: margin.round() as u32,
                });
                if self.editor.history().last().copied() == Some("Content-Aware Fill") {
                    self.status = format!("Content-Aware Fill took {:.1} s", t.elapsed().as_secs_f32());
                }
            }
            return;
        }
        let title = match &d {
            Dialog::SelectEdge(op, _) => match op {
                EdgeOp::Expand(_) => "Expand selection",
                EdgeOp::Contract(_) => "Contract selection",
                EdgeOp::Border(_) => "Border selection",
                EdgeOp::Smooth(_) => "Smooth selection",
            },
            Dialog::Fill(..) => "Fill",
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
        // The primary action's label: a verb for what OK does.
        let primary = match &d {
            Dialog::SelectEdge(..) => "OK",
            Dialog::Fill(_, _, 0) => "Fill",
            Dialog::Fill(..) => "",
            Dialog::ExportJpeg(..) => "Export",
            Dialog::New(..) => "Create",
            Dialog::Preferences(..) => "Save",
            Dialog::ColorRange(..) => "Select",
            Dialog::Filter(_) | Dialog::CanvasSize(..) | Dialog::ImageSize(..) => "Apply",
            Dialog::ConfirmClose | Dialog::ConfirmCloseTab(_) | Dialog::Recover | Dialog::About => "",
        };
        // Enter confirms and Esc cancels, unless a field is being typed in
        // (the first Enter commits the field) or a shortcut is being
        // recorded in Preferences.
        let capturing = matches!(&d, Dialog::Preferences(_, Some(_)));
        let typing = ctx.wants_keyboard_input();
        let (enter, esc) = if capturing || typing {
            (false, false)
        } else {
            ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)))
        };
        // Modal: block the panels and canvas behind the dialog. Preview
        // dialogs keep the canvas undimmed so the preview can be judged.
        let previews = matches!(
            &d,
            Dialog::Filter(_) | Dialog::ColorRange(..) | Dialog::SelectEdge(..)
        );
        modal_backdrop(ctx, !previews);

        let mut keep = true;
        let mut confirmed = false;
        let mut filter_changed = false;
        let shown = egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(DIALOG_MARGIN)))
            .show(ctx, |ui| {
                titled(ui, title, |ui| {
                    raise_controls(ui);
                    // Three-button footers need more room than the content.
                    let (min_w, max_w) = match d {
                        Dialog::Preferences(..) => (320.0, 480.0),
                        Dialog::ConfirmClose | Dialog::ConfirmCloseTab(_) => (420.0, 440.0),
                        _ => (320.0, 360.0),
                    };
                    ui.set_min_width(min_w);
                    ui.set_max_width(max_w);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    match &mut d {
                        Dialog::SelectEdge(op, previewed) => {
                            let (label, max, what) = match op {
                                EdgeOp::Expand(_) => ("Expand by", 100.0, "Grows the selection outward; corners round."),
                                EdgeOp::Contract(_) => ("Contract by", 100.0, "Shrinks the selection inward."),
                                EdgeOp::Border(_) => ("Width", 200.0, "Selects a band centred on the selection's edge."),
                                EdgeOp::Smooth(_) => ("Sample radius", 100.0, "Rounds corners and drops specks and pinholes."),
                            };
                            let mut v = op.amount();
                            let before = v;
                            let o = RowOpts {
                                int: true,
                                log: true,
                                ..RowOpts::default()
                            };
                            slider_row_ex(ui, label, &mut v, 1.0..=max, " px", o);
                            *op = match op {
                                EdgeOp::Expand(_) => EdgeOp::Expand(v),
                                EdgeOp::Contract(_) => EdgeOp::Contract(v),
                                EdgeOp::Border(_) => EdgeOp::Border(v),
                                EdgeOp::Smooth(_) => EdgeOp::Smooth(v),
                            };
                            note(ui, what);
                            if v != before || !*previewed {
                                // Replace the previous preview step rather than
                                // reshaping its result again.
                                if *previewed {
                                    self.undo_quietly();
                                }
                                let cmd = ModifySelectionEdge { op: *op };
                                self.run(&cmd);
                                *previewed = self.editor.history().last().copied() == Some(cmd.label().as_str());
                            }
                        }
                        Dialog::Fill(aware, margin, phase) => {
                            if *phase >= 1 {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                                    ui.label(RichText::new("Filling the selection from its surroundings…").color(TEXT));
                                });
                                *phase = 2;
                                ui.ctx().request_repaint();
                            } else {
                                let can_aware = self.editor.doc().selection.is_some();
                                ui.horizontal(|ui| {
                                    row_label(ui, "Contents", LABEL_W);
                                    ui.vertical(|ui| {
                                        let pick = |ui: &mut egui::Ui, on: bool, text: &str| {
                                            ui.radio(on, text).clicked()
                                        };
                                        if pick(ui, !*aware, "Brush colour") {
                                            *aware = false;
                                        }
                                        let r = ui.add_enabled_ui(can_aware, |ui| pick(ui, *aware, "Content-Aware"));
                                        if r.inner {
                                            *aware = true;
                                        }
                                    });
                                });
                                if *aware {
                                    let o = RowOpts {
                                        int: true,
                                        log: true,
                                        ..RowOpts::default()
                                    };
                                    slider_row_ex(ui, "Sample area", margin, 16.0..=1000.0, " px", o);
                                    note(
                                        ui,
                                        "Rebuilds the selected area from texture found up to this \
                                         far around it, on the active layer.",
                                    );
                                } else if can_aware {
                                    note(ui, "Fills the selection with the brush colour.");
                                } else {
                                    note(ui, "Fills the layer with the brush colour. Select an area to use Content-Aware.");
                                }
                            }
                        }
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
                                ui.label(RichText::new("Free photo editor — GPL-3.0").color(MUTED));
                                ui.add_space(8.0);
                                if ui.add(primary_button("Close")).clicked() || enter || esc {
                                    keep = false;
                                }
                            });
                        }
                        Dialog::ColorRange(tol, previewed) => {
                            note(ui, "Selects everything close to the brush colour.");
                            let changed = {
                                let before = *tol;
                                slider_row(ui, "Fuzziness", tol, 1.0..=100.0, "%");
                                *tol != before
                            };
                            // The colour being matched, editable (and sampleable) in place.
                            let recolored = ui
                                .horizontal(|ui| {
                                    ui.add_sized(
                                        [LABEL_W, 18.0],
                                        egui::Label::new(RichText::new("Colour").color(MUTED)),
                                    );
                                    crate::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb)
                                        .changed()
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
                            let wide = RowOpts {
                                label_w: 120.0,
                                int: true,
                                ..RowOpts::default()
                            };
                            section_title(ui, "GENERAL");
                            let mut steps = p.undo_steps as f32;
                            slider_row_ex(ui, "Undo steps", &mut steps, 1.0..=1000.0, "", wide);
                            p.undo_steps = steps.round().max(1.0) as usize;
                            let mut mb = p.undo_memory_mb as f32;
                            let log = RowOpts { log: true, ..wide };
                            slider_row_ex(ui, "Undo memory", &mut mb, 64.0..=8192.0, " MB", log);
                            p.undo_memory_mb = mb.round() as usize;
                            let mut secs = p.autosave_secs as f32;
                            slider_row_ex(ui, "Autosave every", &mut secs, 15.0..=600.0, " s", wide);
                            p.autosave_secs = secs.round() as u64;
                            ui.horizontal(|ui| {
                                row_label(ui, "Canvas surround", wide.label_w);
                                for (name, c) in [
                                    ("Graphite", [0x14u8, 0x16, 0x19]),
                                    ("Black", [0x00, 0x00, 0x00]),
                                    ("Grey", [0x80, 0x80, 0x80]),
                                    ("Light", [0xD8, 0xD8, 0xD8]),
                                ] {
                                    let on = p.canvas_bg == c;
                                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                                    let ring = if on {
                                        Stroke::new(2.0, ACCENT)
                                    } else if resp.hovered() {
                                        Stroke::new(1.0, MUTED)
                                    } else {
                                        Stroke::new(1.0, LINE)
                                    };
                                    ui.painter().rect_filled(
                                        r.shrink(3.0),
                                        3.0,
                                        Color32::from_rgb(c[0], c[1], c[2]),
                                    );
                                    ui.painter().rect_stroke(r.shrink(1.0), 4.0, ring);
                                    focus_ring(ui, &resp, r, 4.0);
                                    resp.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::RadioButton,
                                            true,
                                            on,
                                            name,
                                        )
                                    });
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
                            ui.add_space(4.0);
                            section_title(ui, "SHORTCUTS");
                            note(ui, "Click a shortcut, then press the new keys (Esc cancels).");
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
                            // The shortcut list scrolls so the footer stays on screen in
                            // short windows (the dialog is anchored to the centre).
                            let list_h = (ui.ctx().screen_rect().height() - 430.0).clamp(110.0, 560.0);
                            let scroll_out = egui::ScrollArea::vertical()
                                .id_salt("prefs-shortcuts")
                                .max_height(list_h)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for (id, label, ..) in session::SHORTCUTS {
                                        ui.horizontal(|ui| {
                                            row_label(ui, label, wide.label_w);
                                            let text = if capturing.as_deref() == Some(*id) {
                                                "press keys…".to_string()
                                            } else {
                                                session::chord_label(ui.ctx(), p, id)
                                            };
                                            let highlight = capturing.as_deref() == Some(*id);
                                            let btn = egui::Button::new(RichText::new(text).monospace())
                                                .min_size(egui::vec2(120.0, 22.0))
                                                .fill(if highlight { ACCENT_TINT } else { GROUND })
                                                .stroke(Stroke::new(
                                                    1.0,
                                                    if highlight { ACCENT } else { LINE },
                                                ));
                                            if ui
                                                .add(btn)
                                                .on_hover_text(format!(
                                                    "Click, then press the new keys for {label}"
                                                ))
                                                .clicked()
                                            {
                                                *capturing = Some(id.to_string());
                                            }
                                            if p.shortcuts.contains_key(*id)
                                                && ui
                                                    .small_button("Reset")
                                                    .on_hover_text("Back to the default shortcut")
                                                    .clicked()
                                            {
                                                p.shortcuts.remove(*id);
                                            }
                                        });
                                    }
                                });
                            a11y_scroll(ui.ctx(), &scroll_out, "Shortcuts");
                        }
                        Dialog::Recover => {
                            note(
                                ui,
                                "The previous session left an autosaved backup, probably after a crash.",
                            );
                            footer(ui, |ui| {
                                if ui.add(primary_button("Recover")).clicked() || enter {
                                    self.recover_autosave();
                                    keep = false;
                                }
                                if ui
                                    .add(footer_button("Discard backup"))
                                    .on_hover_text("Delete the backup and start fresh")
                                    .clicked()
                                {
                                    session::remove_autosave();
                                    keep = false;
                                }
                            });
                        }
                        Dialog::ConfirmClose => {
                            note(ui, "The document has unsaved changes.");
                            footer(ui, |ui| {
                                if ui.add(primary_button("Save and quit")).clicked() || enter {
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
                                if ui.add(footer_button("Cancel")).clicked() || esc {
                                    keep = false;
                                }
                                ui.add_space(16.0);
                                if ui
                                    .add(footer_button("Quit without saving"))
                                    .on_hover_text("Discard the changes and quit")
                                    .clicked()
                                {
                                    self.allow_close = true;
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                    keep = false;
                                }
                            });
                        }
                        Dialog::ConfirmCloseTab(i) => {
                            let i = *i;
                            note(ui, "This document has unsaved changes.");
                            footer(ui, |ui| {
                                if ui.add(primary_button("Save and close")).clicked() || enter {
                                    match self.path.clone() {
                                        Some(p) => self.save_path(&p.to_string_lossy()),
                                        None => self.pick_save(),
                                    }
                                    if self.editor.history().len() == self.saved_rev {
                                        self.force_close_tab(i);
                                    }
                                    keep = false;
                                }
                                if ui.add(footer_button("Cancel")).clicked() || esc {
                                    keep = false;
                                }
                                ui.add_space(16.0);
                                if ui
                                    .add(footer_button("Close without saving"))
                                    .on_hover_text("Discard the changes and close the document")
                                    .clicked()
                                {
                                    self.force_close_tab(i);
                                    keep = false;
                                }
                            });
                        }
                        Dialog::ExportJpeg(p, q) => {
                            ui.label(RichText::new(file_name(p)).monospace().color(MUTED));
                            let mut qf = *q as f32;
                            let o = RowOpts {
                                int: true,
                                ..RowOpts::default()
                            };
                            slider_row_ex(ui, "Quality", &mut qf, 1.0..=100.0, "", o);
                            *q = qf.round().clamp(1.0, 100.0) as u8;
                            note(ui, "Transparent areas are flattened onto white.");
                        }
                        Dialog::New(w, h) => {
                            field_row(
                                ui,
                                "Width",
                                egui::DragValue::new(w).range(1..=16384).suffix(" px"),
                            );
                            field_row(
                                ui,
                                "Height",
                                egui::DragValue::new(h).range(1..=16384).suffix(" px"),
                            );
                        }
                        Dialog::Filter(f) => {
                            let row = |ui: &mut egui::Ui,
                                       label: &str,
                                       v: &mut f32,
                                       r: RangeInclusive<f32>,
                                       sfx: &str,
                                       o: RowOpts| {
                                let before = *v;
                                slider_row_ex(ui, label, v, r, sfx, o);
                                *v != before
                            };
                            // Amounts read as percentages, like the other 0–1 values.
                            let pct = |ui: &mut egui::Ui, v: &mut f32, r: RangeInclusive<f32>| {
                                let before = *v;
                                slider_row_scaled(ui, "Amount", v, r, 100.0, "%");
                                *v != before
                            };
                            let lin = RowOpts::default();
                            let log = RowOpts { log: true, ..lin };
                            match f {
                                Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
                                    filter_changed |= row(ui, "Radius", radius, 0.5..=60.0, " px", log);
                                }
                                Filter::Sharpen { amount, radius } => {
                                    filter_changed |= pct(ui, amount, 0.0..=5.0);
                                    filter_changed |= row(ui, "Radius", radius, 0.5..=20.0, " px", lin);
                                }
                                Filter::Noise { amount } => {
                                    filter_changed |= pct(ui, amount, 0.0..=1.0);
                                }
                                Filter::MotionBlur { angle, distance } => {
                                    filter_changed |= row(ui, "Angle", angle, -180.0..=180.0, "°", lin);
                                    filter_changed |= row(ui, "Distance", distance, 1.0..=200.0, " px", lin);
                                }
                                Filter::Median { radius } => {
                                    let int = RowOpts { int: true, ..lin };
                                    filter_changed |= row(ui, "Radius", radius, 1.0..=8.0, " px", int);
                                }
                                Filter::HighPass { radius } => {
                                    filter_changed |= row(ui, "Radius", radius, 0.5..=60.0, " px", log);
                                }
                                Filter::Mosaic { size } => {
                                    let o = RowOpts { int: true, ..log };
                                    filter_changed |= row(ui, "Cell size", size, 2.0..=200.0, " px", o);
                                }
                                Filter::Emboss {
                                    angle,
                                    height,
                                    amount,
                                } => {
                                    filter_changed |= row(ui, "Angle", angle, -180.0..=180.0, "°", lin);
                                    filter_changed |= row(ui, "Height", height, 1.0..=10.0, " px", lin);
                                    filter_changed |= pct(ui, amount, 0.0..=5.0);
                                }
                                Filter::FindEdges => {
                                    note(ui, "No settings: every channel's edges, dark on white.");
                                }
                                Filter::SurfaceBlur { radius, threshold } => {
                                    let o = RowOpts { int: true, ..log };
                                    filter_changed |= row(ui, "Radius", radius, 1.0..=100.0, " px", o);
                                    let o = RowOpts { int: true, ..lin };
                                    filter_changed |= row(ui, "Threshold", threshold, 2.0..=255.0, " levels", o);
                                }
                                Filter::LensBlur { radius, highlights } => {
                                    filter_changed |= row(ui, "Radius", radius, 1.0..=100.0, " px", log);
                                    let before = *highlights;
                                    slider_row_scaled(ui, "Highlights", highlights, 0.0..=1.0, 100.0, "%");
                                    filter_changed |= *highlights != before;
                                }
                                Filter::DustScratches { radius, threshold } => {
                                    let o = RowOpts { int: true, ..lin };
                                    filter_changed |= row(ui, "Radius", radius, 1.0..=8.0, " px", o);
                                    filter_changed |= row(ui, "Threshold", threshold, 0.0..=255.0, " levels", o);
                                }
                            }
                            note(ui, "Previewed on the canvas; applies to the active layer.");
                        }
                        Dialog::CanvasSize(w, h, anchor) => {
                            field_row(
                                ui,
                                "Width",
                                egui::DragValue::new(w).range(1..=16384).suffix(" px"),
                            );
                            field_row(
                                ui,
                                "Height",
                                egui::DragValue::new(h).range(1..=16384).suffix(" px"),
                            );
                            ui.horizontal(|ui| {
                                row_label(ui, "Anchor", LABEL_W);
                                anchor_grid(ui, anchor);
                            });
                        }
                        Dialog::ImageSize(w, h, lock) => {
                            let (ow, oh) = (self.editor.doc().width as f32, self.editor.doc().height as f32);
                            let rw = field_row(
                                ui,
                                "Width",
                                egui::DragValue::new(w).range(1..=16384).suffix(" px"),
                            );
                            let rh = field_row(
                                ui,
                                "Height",
                                egui::DragValue::new(h).range(1..=16384).suffix(" px"),
                            );
                            if *lock {
                                if rw.changed() {
                                    *h = ((*w as f32) * oh / ow).round().max(1.0) as u32;
                                } else if rh.changed() {
                                    *w = ((*h as f32) * ow / oh).round().max(1.0) as u32;
                                }
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(LABEL_W + ui.spacing().item_spacing.x);
                                check(ui, lock, "Keep aspect ratio");
                            });
                            note(ui, "Resamples every layer bilinearly.");
                        }
                    }
                    if !primary.is_empty() {
                        footer(ui, |ui| {
                            if ui.add(primary_button(primary)).clicked() || enter {
                                confirmed = true;
                            }
                            if ui.add(footer_button("Cancel")).clicked() || esc {
                                keep = false;
                            }
                        });
                    }
                })
            });
        // The dialog above the backdrop, which is above everything else.
        if let Some(shown) = shown {
            ctx.move_to_top(shown.response.layer_id);
            ctx.accesskit_node_builder(shown.response.id, |b| {
                b.set_role(egui::accesskit::Role::Dialog);
                b.set_name(title);
            });
        }

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

        // Content-aware fill can take a moment: show "Filling" for a frame
        // first, and run it on the next (see the top of this function).
        if confirmed {
            if let Dialog::Fill(true, _, phase @ 0) = &mut d {
                *phase = 1;
                confirmed = false;
                ctx.request_repaint();
            }
        }
        if confirmed {
            match &d {
                Dialog::ConfirmClose | Dialog::ConfirmCloseTab(_) | Dialog::Recover | Dialog::About => {}
                // The previewed step already is the result.
                Dialog::SelectEdge(..) => {}
                Dialog::Fill(..) => self.fill_active(),
                Dialog::ColorRange(..) => {}
                Dialog::Preferences(p, _) => {
                    // The history strip's fold state lives in the prefs but
                    // isn't edited here; keep whatever it is now.
                    let folded = self.prefs.history_collapsed;
                    self.prefs = p.clone();
                    self.prefs.history_collapsed = folded;
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
            if let Dialog::SelectEdge(_, true) = d {
                if !confirmed {
                    self.undo_quietly();
                }
            }
            self.filter_previewed = false;
        }
    }
}

impl App {
    /// Step back once without reporting it: a dialog withdrawing its own
    /// preview step.
    fn undo_quietly(&mut self) {
        if self.editor.undo().is_some() {
            self.below.note_change(self.editor.doc(), None);
            let r = self.editor.last_affected();
            self.mark(r);
        }
    }

    /// Content-aware fill's default sampling margin for the selection:
    /// the hole's larger side, at least 64 px.
    pub(crate) fn content_aware_margin(&self) -> f32 {
        let doc = self.editor.doc();
        let b = doc
            .selection
            .as_ref()
            .map_or(Rect::default(), |s| s.tight_bounds(doc.canvas()));
        (b.w.max(b.h) as f32).clamp(64.0, 1000.0)
    }
}

/// A full-window layer under a dialog that swallows clicks, so nothing
/// behind it can be edited while the dialog is open; optionally dimmed.
/// It sits in the foreground order on top of the canvas's floating bars
/// (zoom, selection actions); the dialog is then raised above it.
fn modal_backdrop(ctx: &egui::Context, dim: bool) {
    let id = egui::Id::new("modal-backdrop");
    let screen = ctx.screen_rect();
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .sense(BACKDROP_SENSE)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let blocker = Sense {
                drag: true,
                ..BACKDROP_SENSE
            };
            let (r, _) = ui.allocate_exact_size(screen.size(), blocker);
            if dim {
                ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(110));
            }
        });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
}

const DIALOG_MARGIN: f32 = 16.0;

/// A dialog's body under its centred title and hairline, laid out like
/// egui's window title bar (title row, a gap of both frame margins, the
/// hairline one margin below the title text). egui's own bar is a focusable
/// control with no name, and the first Tab stop; this title is plain paint.
fn titled<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let galley = egui::WidgetText::from(title).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Heading,
    );
    let ink = ui.visuals().text_color();
    let hairline = ui.visuals().widgets.noninteractive.bg_stroke;
    let h = galley.size().y.max(ui.spacing().interact_size.y);
    let spacing = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    let (row, _) = ui.allocate_exact_size(Vec2::new(galley.size().x, h), Sense::hover());
    ui.add_space(2.0 * DIALOG_MARGIN);
    ui.spacing_mut().item_spacing.y = spacing;
    let line_y = row.top() + galley.size().y + DIALOG_MARGIN;
    let out = body(ui);
    let full = ui.min_rect();
    let p = ui.ctx().layer_painter(ui.layer_id());
    let head = egui::Rect::from_x_y_ranges(full.x_range(), row.y_range());
    let pos = Align2::CENTER_CENTER
        .align_size_within_rect(galley.size(), head)
        .min;
    p.galley(pos, galley, ink);
    p.hline(full.x_range().expand(DIALOG_MARGIN).shrink(0.1), line_y, hairline);
    out
}

/// A muted explanatory line, wrapped to the dialog width.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(MUTED)).wrap());
}

/// The dialog footer: a hairline, then buttons laid right to left (add
/// the primary action first so it sits at the far right).
fn footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    // As wide as the content above, never wider: a full-width layout
    // would stretch the auto-sized window to its maximum.
    let w = ui.min_rect().width();
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, LINE));
    ui.add_space(2.0);
    ui.allocate_ui_with_layout(
        egui::vec2(w, 28.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        },
    );
}

/// The canvas-size anchor: a 3×3 grid of cells, the chosen one filled.
fn anchor_grid(ui: &mut egui::Ui, anchor: &mut (f32, f32)) {
    const NAMES: [[&str; 3]; 3] = [
        ["top left", "top", "top right"],
        ["left", "centre", "right"],
        ["bottom left", "bottom", "bottom right"],
    ];
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        for (row, names) in NAMES.iter().enumerate() {
            ui.horizontal(|ui| {
                for (col, name) in names.iter().enumerate() {
                    let a = (col as f32 * 0.5, row as f32 * 0.5);
                    let on = (anchor.0 - a.0).abs() < 1e-3 && (anchor.1 - a.1).abs() < 1e-3;
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                    let fill = if on {
                        ACCENT
                    } else if resp.hovered() {
                        HOVER
                    } else {
                        GROUND
                    };
                    ui.painter().rect_filled(r, 4.0, fill);
                    ui.painter()
                        .rect_stroke(r, 4.0, Stroke::new(1.0, if on { ACCENT } else { LINE }));
                    if on {
                        ui.painter().circle_filled(r.center(), 4.0, ACCENT_INK);
                    }
                    focus_ring(ui, &resp, r, 4.0);
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::RadioButton,
                            true,
                            on,
                            format!("Anchor {name}"),
                        )
                    });
                    if resp.on_hover_text(format!("Anchor {name}")).clicked() {
                        *anchor = a;
                    }
                }
            });
        }
    });
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
