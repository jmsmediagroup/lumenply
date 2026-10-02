use super::*;

pub(crate) enum Dialog {
    ExportJpeg(String, u8),
    New(u32, u32),
    Filter(Filter),
    CanvasSize(u32, u32, (f32, f32)),
    ImageSize(u32, u32, bool),
    /// The window close was intercepted because of unsaved changes.
    ConfirmClose,
    /// An autosave backup from a previous session was found at startup.
    Recover,
}

impl App {
    fn file_dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        d
    }

    pub(crate) fn pick_open(&mut self) {
        if let Some(p) = self
            .file_dialog()
            .add_filter(
                "Projects & images",
                &["nge", "psd", "ora", "png", "jpg", "jpeg", "tif", "tiff"],
            )
            .pick_file()
        {
            self.open_path(&p.to_string_lossy());
        }
    }

    pub(crate) fn pick_open_image(&mut self) {
        if let Some(p) = self
            .file_dialog()
            .add_filter("Images", &["png", "jpg", "jpeg", "tif", "tiff"])
            .pick_file()
        {
            self.open_image(&p.to_string_lossy());
        }
    }

    pub(crate) fn pick_place(&mut self) {
        if let Some(p) = self
            .file_dialog()
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .pick_file()
        {
            self.place_image(&p.to_string_lossy());
        }
    }

    /// Native save panel; returns the chosen path with `ext` enforced.
    fn pick_save_path(&self, what: &str, ext: &str) -> Option<String> {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        self.file_dialog()
            .add_filter(what, &[ext])
            .set_file_name(format!("{name}.{ext}"))
            .save_file()
            .map(|p| {
                if p.extension().is_some() {
                    p.to_string_lossy().into_owned()
                } else {
                    p.with_extension(ext).to_string_lossy().into_owned()
                }
            })
    }

    pub(crate) fn pick_save(&mut self) {
        if let Some(p) = self.pick_save_path("NGE project", "nge") {
            self.save_path(&p);
        }
    }

    pub(crate) fn pick_export_png(&mut self) {
        if let Some(p) = self.pick_save_path("PNG image", "png") {
            self.export_png(&p);
        }
    }

    pub(crate) fn pick_export_psd(&mut self) {
        if let Some(p) = self.pick_save_path("Photoshop PSD", "psd") {
            self.export_psd(&p);
        }
    }

    pub(crate) fn pick_export_16bit(&mut self) {
        let picked = self
            .file_dialog()
            .add_filter("16-bit PNG", &["png"])
            .add_filter("16-bit TIFF", &["tif", "tiff"])
            .set_file_name(format!(
                "{}.png",
                self.path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "untitled".into())
            ))
            .save_file();
        if let Some(p) = picked {
            let p = if p.extension().is_some() {
                p
            } else {
                p.with_extension("png")
            };
            let flat = nge_render::composite_raster(self.editor.doc());
            match nge_io::save_16bit(&p, &flat) {
                Ok(()) => self.status = format!("Exported {}", p.display()),
                Err(e) => self.status = format!("Could not export: {e}"),
            }
        }
    }

    pub(crate) fn pick_export_ora(&mut self) {
        if let Some(p) = self.pick_save_path("OpenRaster", "ora") {
            self.export_ora(&p);
        }
    }

    pub(crate) fn export_ora(&mut self, path: &str) {
        match nge_io::ora::save(path, self.editor.doc()) {
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
        if let Some(p) = self.pick_save_path("JPEG image", "jpg") {
            self.dialog = Some(Dialog::ExportJpeg(p, 90));
        }
    }
}

impl App {
    // ---- files -----------------------------------------------------------------

    pub(crate) fn open_path(&mut self, path: &str) {
        if is_ora_path(path) {
            match nge_io::ora::load(path) {
                Ok(rep) => {
                    let n = rep.warnings.len();
                    self.set_doc(Editor::new(rep.value), None);
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
            match nge_io::psd::load(path) {
                Ok(rep) => {
                    let n = rep.warnings.len();
                    self.set_doc(Editor::new(rep.value), None);
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
                self.set_doc(Editor::new(doc), Some(PathBuf::from(path)));
                self.recent = session::push_recent(path);
                self.status = format!("Opened {path}");
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    pub(crate) fn export_psd(&mut self, path: &str) {
        match nge_io::psd::save(path, self.editor.doc()) {
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
        match nge_io::load(path) {
            Ok(raster) => {
                let (w, h) = (raster.width, raster.height);
                let mut ed = Editor::new(Document::new(w, h));
                let _ = ed.execute(&AddPixelLayer::from_raster(file_name(path), raster, 0, 0));
                self.set_doc(ed, None);
                self.recent = session::push_recent(path);
                self.status = format!("Opened {path} ({w}×{h})");
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    pub(crate) fn place_image(&mut self, path: &str) {
        match nge_io::load(path) {
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
        let flat = nge_render::composite_raster(self.editor.doc());
        match nge_io::save_png(path, &flat) {
            Ok(()) => self.status = format!("Exported {path}"),
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    pub(crate) fn export_jpeg(&mut self, path: &str, quality: u8) {
        let flat = nge_render::composite_raster(self.editor.doc());
        match nge_io::save_jpeg(path, &flat, quality) {
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
            Dialog::Recover => "Recover autosaved document",
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
                    Dialog::Recover => {
                        ui.label("The previous session left an autosaved backup,");
                        ui.label("probably after a crash.");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Recover").clicked() {
                                if let Some(file) = session::autosave_file() {
                                    let source = session::autosave_source();
                                    match project::load(&file) {
                                        Ok(doc) => {
                                            self.set_doc(Editor::new(doc), source);
                                            // Recovered work is unsaved by definition.
                                            self.saved_rev = usize::MAX;
                                            self.status = "Recovered the autosaved document".into();
                                        }
                                        Err(e) => self.status = format!("Could not recover: {e}"),
                                    }
                                }
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
                if !matches!(d, Dialog::ConfirmClose | Dialog::Recover) {
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
                Dialog::ConfirmClose | Dialog::Recover => {}
                Dialog::ExportJpeg(p, q) => self.export_jpeg(p, *q),
                Dialog::New(w, h) => self.set_doc(blank(*w, *h), None),
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
            if matches!(d, Dialog::Filter(_)) {
                // Drop the preview whether confirmed or cancelled.
                self.mark(None);
            }
            self.filter_previewed = false;
        }
    }
}
