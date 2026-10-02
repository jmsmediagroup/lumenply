use super::*;

impl App {
    // ---- edits used by menus and panels ----------------------------------------------

    pub(crate) fn undo(&mut self) {
        if let Some(l) = self.editor.undo() {
            self.status = format!("Undid {l}");
            self.below.note_change(self.editor.doc(), None);
            let r = self.editor.last_affected();
            self.mark(r);
            self.fix_active();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(l) = self.editor.redo() {
            self.status = format!("Redid {l}");
            self.below.note_change(self.editor.doc(), None);
            let r = self.editor.last_affected();
            self.mark(r);
            self.fix_active();
        }
    }

    pub(crate) fn fill_active(&mut self) {
        if let Some(layer) = self.active {
            let color = self.make_brush().color;
            self.run(&Fill { layer, color });
        }
    }

    pub(crate) fn clear_active(&mut self) {
        if let Some(layer) = self.active {
            self.run(&Clear { layer });
        }
    }

    pub(crate) fn reorder_active(&mut self, delta: i32) {
        if let Some(layer) = self.active {
            self.run(&ReorderLayer { layer, delta });
        }
    }

    pub(crate) fn flip_active(&mut self, horizontal: bool) {
        if let Some(layer) = self.active {
            self.run(&FlipLayer { layer, horizontal });
        }
    }

    pub(crate) fn add_pixel_layer(&mut self) {
        let n = self.editor.doc().layer_count() + 1;
        self.run(&AddPixelLayer::new(format!("Layer {n}")));
        self.select_top();
    }

    pub(crate) fn add_adjustment(&mut self, adj: Adjustment) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddAdjustmentLayer::new(adj);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    pub(crate) fn add_filter_layer(&mut self, f: Filter) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddFilterLayer::new(f);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    pub(crate) fn delete_active(&mut self) {
        if let Some(id) = self.active {
            self.run(&RemoveLayer { layer: id });
            self.select_top();
        }
    }

    /// Group the multi-selection, or the active layer with the one below it.
    pub(crate) fn group_selected(&mut self) {
        let doc = self.editor.doc();
        let mut ids: Vec<LayerId> = self.selected.clone();
        if ids.len() < 2 {
            let Some(active) = self.active else { return };
            let parent = doc.parent_of(active);
            let siblings: Vec<LayerId> = match parent {
                None => doc.layers().iter().map(|l| l.id).collect(),
                Some(p) => doc.layer(p).map_or(Vec::new(), |g| {
                    g.children().unwrap_or(&[]).iter().map(|l| l.id).collect()
                }),
            };
            let i = siblings.iter().position(|id| *id == active).unwrap_or(0);
            ids = vec![active];
            if i > 0 {
                ids.push(siblings[i - 1]);
            }
        }
        let new_id = doc.next_id();
        let n = doc.layer_count();
        self.run(&GroupLayers {
            layers: ids,
            name: format!("Group {}", n),
        });
        if self.editor.doc().layer(new_id).is_some() {
            self.set_active(Some(new_id));
        }
    }

    pub(crate) fn ungroup_active(&mut self) {
        if let Some(id) = self.active {
            self.run(&UngroupLayer { layer: id });
            self.select_top();
        }
    }

    pub(crate) fn begin_free_transform(&mut self) {
        let Some(id) = self.active else { return };
        // Pixel layers and smart objects transform; a smart object's
        // bounds come from its rendered cache.
        let Some(b) = self
            .active_layer()
            .filter(|l| l.pixels().is_some() || l.smart_layer().is_some())
            .and_then(|l| l.raster_store())
            .and_then(|p| p.content_bounds())
        else {
            self.status = "Free transform needs a pixel layer or smart object with content".into();
            return;
        };
        self.tool = Tool::Move;
        self.xform = Some(Xform {
            layer: id,
            bounds: b,
            sx: 1.0,
            sy: 1.0,
            shear: 0.0,
            angle: 0.0,
            dx: 0.0,
            dy: 0.0,
            base: (1.0, 1.0, 0.0, 0.0, 0.0),
            quad: None,
            qbase: [(0.0, 0.0); 4],
            warp: None,
            wbase: Vec::new(),
            last_preview: b,
        });
        self.status =
            "Free transform: corners scale, edges stretch one axis, outside rotates, inside moves".into();
    }

    /// Free transform straight into perspective mode.
    pub(crate) fn begin_perspective(&mut self) {
        if !self.active_is_pixel() {
            self.status = "Perspective needs a pixel layer (rasterize smart objects first)".into();
            return;
        }
        self.begin_free_transform();
        if let Some(x) = self.xform.as_mut() {
            x.quad = Some(x.corners());
            self.status = "Perspective: drag each corner freely; Enter applies".into();
        }
    }

    /// Free transform straight into warp mode.
    pub(crate) fn begin_warp(&mut self) {
        if !self.active_is_pixel() {
            self.status = "Warp needs a pixel layer (rasterize smart objects first)".into();
            return;
        }
        self.begin_free_transform();
        if let Some(x) = self.xform.as_mut() {
            let mesh = x.initial_warp();
            x.warp = Some(mesh);
            self.status = "Warp: drag any mesh point; drag elsewhere moves it all; Enter applies".into();
        }
    }

    pub(crate) fn commit_free_transform(&mut self) {
        if let Some(x) = self.xform.take() {
            if let Some(grid) = x.warp_grid() {
                let identity = lumenply_render::WarpGrid::identity(x.bounds, grid.cols, grid.rows);
                let moved = grid
                    .points
                    .iter()
                    .zip(&identity.points)
                    .any(|(a, b)| (a.0 - b.0).abs() > 1e-3 || (a.1 - b.1).abs() > 1e-3);
                if moved {
                    self.run(&WarpLayer { layer: x.layer, grid });
                }
            } else if let Some(quad) = x.quad {
                let b = x.bounds;
                let identity = [
                    (b.x as f32, b.y as f32),
                    (b.right() as f32, b.y as f32),
                    (b.right() as f32, b.bottom() as f32),
                    (b.x as f32, b.bottom() as f32),
                ];
                if quad != identity {
                    self.run(&PerspectiveLayer { layer: x.layer, quad });
                }
            } else {
                let t = x.affine();
                if t.integer_translation() != Some((0, 0)) {
                    self.run(&TransformLayer {
                        layer: x.layer,
                        transform: t,
                    });
                }
            }
            self.mark(None);
        }
    }

    pub(crate) fn cancel_free_transform(&mut self) {
        if self.xform.take().is_some() {
            self.mark(None);
            self.status = "Transform cancelled".into();
        }
    }

    // ---- menu bar ----------------------------------------------------------------------

    pub(crate) fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu")
            .exact_height(40.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // The mark, top-left; click for About.
                    let (chip, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
                    ui.painter().rect_filled(chip, 7.0, GROUND);
                    brand::paint_mark(ui.painter(), chip.shrink(4.0), TEXT, ACCENT, GROUND);
                    if resp.on_hover_text("About Lumenply").clicked() {
                        self.dialog = Some(Dialog::About);
                    }
                    ui.add_space(2.0);
                    ui.menu_button("File", |ui| {
                        if ui.button("New...").clicked() {
                            self.dialog = Some(Dialog::New(1200, 800));
                            ui.close_menu();
                        }
                        if ui
                            .button("Open...   Ctrl+O")
                            .on_hover_text(".lumen project, .psd, .ora, images")
                            .clicked()
                        {
                            self.pick_open();
                            ui.close_menu();
                        }
                        ui.menu_button("Open recent", |ui| {
                            if self.recent.is_empty() {
                                ui.label(RichText::new("Nothing yet").weak());
                            }
                            let mut open: Option<String> = None;
                            for p in &self.recent {
                                if ui.button(file_name(p)).on_hover_text(p).clicked() {
                                    open = Some(p.clone());
                                    ui.close_menu();
                                }
                            }
                            if let Some(p) = open {
                                self.open_path(&p);
                            }
                        });
                        if ui.button("Open image (PNG/JPEG)...").clicked() {
                            self.pick_open_image();
                            ui.close_menu();
                        }
                        if ui.button("Place image as layer...").clicked() {
                            self.pick_place();
                            ui.close_menu();
                        }
                        if ui.button("Open demo document").clicked() {
                            match lumenply_core::demo::build(1200, 800) {
                                Ok(ed) => self.open_in_new_tab(ed, None),
                                Err(e) => self.status = e.to_string(),
                            }
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Save   Ctrl+S").clicked() {
                            match self.path.clone() {
                                Some(p) => self.save_path(&p.to_string_lossy()),
                                None => self.pick_save(),
                            }
                            ui.close_menu();
                        }
                        if ui.button("Save as...").clicked() {
                            self.pick_save();
                            ui.close_menu();
                        }
                        if ui.button("Export PNG...").clicked() {
                            self.pick_export_png();
                            ui.close_menu();
                        }
                        if ui.button("Export JPEG...").clicked() {
                            self.pick_export_jpeg();
                            ui.close_menu();
                        }
                        if ui.button("Export OpenEXR...").clicked() {
                            self.pick_export_exr();
                            ui.close_menu();
                        }
                        if ui.button("Export 16-bit PNG/TIFF...").clicked() {
                            self.pick_export_16bit();
                            ui.close_menu();
                        }
                        if ui.button("Export OpenRaster...").clicked() {
                            self.pick_export_ora();
                            ui.close_menu();
                        }
                        if ui.button("Export Photoshop PSD (16-bit)...").clicked() {
                            self.pick_export_psd16();
                            ui.close_menu();
                        }
                        if ui.button("Export Photoshop PSD...").clicked() {
                            self.pick_export_psd();
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        if ui
                            .add_enabled(self.editor.can_undo(), egui::Button::new("Undo   Ctrl+Z"))
                            .clicked()
                        {
                            self.undo();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(self.editor.can_redo(), egui::Button::new("Redo   Ctrl+Shift+Z"))
                            .clicked()
                        {
                            self.redo();
                            ui.close_menu();
                        }
                        ui.separator();
                        let pixel = self.active_is_pixel();
                        let smart = self.active_layer().is_some_and(|l| l.smart_layer().is_some());
                        if ui
                            .add_enabled(pixel || smart, egui::Button::new("Free transform   Ctrl+T"))
                            .clicked()
                        {
                            self.begin_free_transform();
                            ui.close_menu();
                        }
                        if ui.add_enabled(pixel, egui::Button::new("Perspective")).clicked() {
                            self.begin_perspective();
                            ui.close_menu();
                        }
                        if ui.add_enabled(pixel, egui::Button::new("Warp")).clicked() {
                            self.begin_warp();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(pixel, egui::Button::new("Fill with brush colour   Shift+F5"))
                            .clicked()
                        {
                            self.fill_active();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(pixel, egui::Button::new("Clear   Delete"))
                            .clicked()
                        {
                            self.clear_active();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Preferences...").clicked() {
                            self.dialog = Some(Dialog::Preferences(self.prefs.clone(), None));
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Image", |ui| {
                        let doc = self.editor.doc();
                        let (w, h) = (doc.width, doc.height);
                        let crop_rect = doc.selection.as_ref().map(|s| s.tight_bounds(doc.canvas()));
                        if ui
                            .add_enabled(
                                crop_rect.is_some_and(|r| !r.is_empty()),
                                egui::Button::new("Crop to selection"),
                            )
                            .clicked()
                        {
                            if let Some(rect) = crop_rect {
                                self.run(&CropDocument { rect });
                            }
                            ui.close_menu();
                        }
                        if ui.button("Auto contrast").clicked() {
                            self.auto_contrast();
                            ui.close_menu();
                        }
                        if ui.button("Auto color").clicked() {
                            self.auto_color();
                            ui.close_menu();
                        }
                        let mut float_mode = self.editor.doc().float_mode;
                        if ui
                            .checkbox(&mut float_mode, "32-bit float (HDR)")
                            .on_hover_text(
                                "Keep values outside 0–1 through every edit (HDR). Off, tiles \
                                 return to 16-bit as they are next edited, clamping the range",
                            )
                            .changed()
                        {
                            self.run(&SetFloatMode { on: float_mode });
                            ui.close_menu();
                        }
                        if ui.button("Canvas size...").clicked() {
                            self.dialog = Some(Dialog::CanvasSize(w, h, (0.5, 0.5)));
                            ui.close_menu();
                        }
                        if ui.button("Image size...").clicked() {
                            self.dialog = Some(Dialog::ImageSize(w, h, true));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Rotate 90° clockwise").clicked() {
                            self.run(&RotateImage { quarter_turns: 1 });
                            self.view_cmd = Some(ViewCmd::Fit);
                            ui.close_menu();
                        }
                        if ui.button("Rotate 90° counter-clockwise").clicked() {
                            self.run(&RotateImage { quarter_turns: -1 });
                            self.view_cmd = Some(ViewCmd::Fit);
                            ui.close_menu();
                        }
                        if ui.button("Rotate 180°").clicked() {
                            self.run(&RotateImage { quarter_turns: 2 });
                            ui.close_menu();
                        }
                        if ui.button("Flip image horizontal").clicked() {
                            self.run(&FlipImage { horizontal: true });
                            ui.close_menu();
                        }
                        if ui.button("Flip image vertical").clicked() {
                            self.run(&FlipImage { horizontal: false });
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Select", |ui| {
                        if ui
                            .selectable_label(self.quick_mask, "Quick mask   Q")
                            .on_hover_text("Paint the selection: white selects, black deselects")
                            .clicked()
                        {
                            self.toggle_quick_mask();
                            ui.close_menu();
                        }
                        if ui
                            .button("Colour range...")
                            .on_hover_text("Select everything close to the brush colour")
                            .clicked()
                        {
                            self.dialog = Some(Dialog::ColorRange(25.0, false));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("All   Ctrl+A").clicked() {
                            self.run(&SetSelection {
                                selection: Some(Selection::all()),
                            });
                            ui.close_menu();
                        }
                        if ui.button("None   Ctrl+D").clicked() {
                            self.run(&SetSelection { selection: None });
                            ui.close_menu();
                        }
                        if ui.button("Invert   Ctrl+Shift+I").clicked() {
                            self.run(&InvertSelection);
                            ui.close_menu();
                        }
                        ui.separator();
                        let has_sel = self.editor.doc().selection.is_some();
                        if ui.add_enabled(has_sel, egui::Button::new("Feather")).clicked() {
                            self.run(&FeatherSelection { radius: self.feather });
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                has_sel && self.active.is_some(),
                                egui::Button::new("Mask active layer"),
                            )
                            .clicked()
                        {
                            if let Some(l) = self.active {
                                self.run(&MaskFromSelection { layer: l });
                            }
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Layer", |ui| {
                        if ui.button("New pixel layer").clicked() {
                            self.add_pixel_layer();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(self.active_is_pixel(), egui::Button::new("Layer via copy"))
                            .on_hover_text("Copy the selection (or the whole layer) onto a new layer")
                            .clicked()
                        {
                            self.run_menu_action("layer-via-copy");
                            ui.close_menu();
                        }
                        ui.menu_button("New adjustment layer", |ui| {
                            for (name, adj) in adjustment_presets() {
                                if ui.button(name).clicked() {
                                    self.add_adjustment(adj);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button("New live filter layer", |ui| {
                            for (name, f) in filter_presets() {
                                if ui.button(name).clicked() {
                                    self.add_filter_layer(f);
                                    ui.close_menu();
                                }
                            }
                        });
                        if ui.button("Delete layer").clicked() {
                            self.delete_active();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Group   Ctrl+G").clicked() {
                            self.group_selected();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(self.active_is_group(), egui::Button::new("Ungroup"))
                            .clicked()
                        {
                            self.ungroup_active();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Move up").clicked() {
                            self.reorder_active(1);
                            ui.close_menu();
                        }
                        if ui.button("Move down").clicked() {
                            self.reorder_active(-1);
                            ui.close_menu();
                        }
                        ui.separator();
                        let has_layer = self.active.is_some();
                        let has_mask = self.active_has_mask();
                        if ui
                            .add_enabled(has_layer && !has_mask, egui::Button::new("Add mask"))
                            .clicked()
                        {
                            if let Some(l) = self.active {
                                self.run(&AddMask { layer: l });
                            }
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(has_mask, egui::Button::new("Remove mask"))
                            .clicked()
                        {
                            if let Some(l) = self.active {
                                self.run(&RemoveMask { layer: l });
                                self.editing_mask = false;
                            }
                            ui.close_menu();
                        }
                        ui.separator();
                        let pixel = self.active_is_pixel();
                        if ui
                            .add_enabled(pixel, egui::Button::new("Flip horizontal"))
                            .clicked()
                        {
                            self.flip_active(true);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(pixel, egui::Button::new("Flip vertical"))
                            .clicked()
                        {
                            self.flip_active(false);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Filter", |ui| {
                        let pixel = self.active_is_pixel();
                        if ui
                            .add_enabled(pixel, egui::Button::new("Gaussian blur..."))
                            .clicked()
                        {
                            self.dialog = Some(Dialog::Filter(Filter::GaussianBlur { radius: 8.0 }));
                            ui.close_menu();
                        }
                        if ui.add_enabled(pixel, egui::Button::new("Box blur...")).clicked() {
                            self.dialog = Some(Dialog::Filter(Filter::BoxBlur { radius: 5.0 }));
                            ui.close_menu();
                        }
                        if ui.add_enabled(pixel, egui::Button::new("Sharpen...")).clicked() {
                            self.dialog = Some(Dialog::Filter(Filter::Sharpen {
                                amount: 1.0,
                                radius: 2.0,
                            }));
                            ui.close_menu();
                        }
                        ui.separator();
                        ui.label(
                            RichText::new("Live (non-destructive) filter layers")
                                .weak()
                                .small(),
                        );
                        for (name, f) in filter_presets() {
                            if ui.button(format!("{name} layer")).clicked() {
                                self.add_filter_layer(f);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button("View", |ui| {
                        if ui.button("Fit on screen   0").clicked() {
                            self.view_cmd = Some(ViewCmd::Fit);
                            ui.close_menu();
                        }
                        if ui.button("Actual pixels   1").clicked() {
                            self.view_cmd = Some(ViewCmd::Actual);
                            ui.close_menu();
                        }
                    });

                    ui.add_space(10.0);
                    self.document_tab(ui);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let export = egui::Button::new(RichText::new("Export").color(ACCENT_INK).strong())
                            .fill(ACCENT);
                        egui::menu::menu_custom_button(ui, export, |ui| {
                            if ui.button("PNG...").clicked() {
                                self.pick_export_png();
                                ui.close_menu();
                            }
                            if ui.button("JPEG...").clicked() {
                                self.pick_export_jpeg();
                                ui.close_menu();
                            }
                            if ui.button("PSD...").clicked() {
                                self.pick_export_psd();
                                ui.close_menu();
                            }
                            if ui.button("OpenRaster...").clicked() {
                                self.pick_export_ora();
                                ui.close_menu();
                            }
                            if ui.button("16-bit PNG/TIFF...").clicked() {
                                self.pick_export_16bit();
                                ui.close_menu();
                            }
                            if ui.button("OpenEXR...").clicked() {
                                self.pick_export_exr();
                                ui.close_menu();
                            }
                        });
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Search tools, filters...   Ctrl K").color(MUTED),
                                )
                                .fill(GROUND)
                                .stroke(Stroke::new(1.0, LINE))
                                .min_size(egui::vec2(230.0, 26.0)),
                            )
                            .clicked()
                        {
                            self.toggle_palette();
                        }
                    });
                });
            });
    }
}

impl App {
    /// The open documents as tabs: the live one raised with an accent
    /// underline, the rest flat; unsaved dot, middle-click or × to close,
    /// and a + for a new blank document.
    fn document_tab(&mut self, ui: &mut egui::Ui) {
        let infos = self.tab_infos();
        let several = infos.len() > 1;
        let mut switch = None;
        let mut close = None;
        for (i, (name, unsaved)) in infos.into_iter().enumerate() {
            let live = i == self.cur_tab;
            let label = if unsaved {
                format!("{name}  ")
            } else {
                name.clone()
            };
            let text = RichText::new(label).color(if live { TEXT } else { MUTED });
            let resp = ui.add(
                egui::Button::new(text)
                    .fill(if live { RAISED } else { Color32::TRANSPARENT })
                    .stroke(Stroke::new(1.0, if live { LINE } else { Color32::TRANSPARENT }))
                    .rounding(6.0),
            );
            if live {
                ui.painter().line_segment(
                    [
                        resp.rect.left_bottom() + egui::vec2(4.0, 0.0),
                        resp.rect.right_bottom() + egui::vec2(-4.0, 0.0),
                    ],
                    Stroke::new(2.0, ACCENT),
                );
            }
            if unsaved {
                let c = resp.rect.right_center() + egui::vec2(-9.0, 0.0);
                ui.painter().circle_filled(c, 3.0, ACCENT);
            }
            let resp = resp.on_hover_text(if unsaved {
                "Unsaved changes — middle-click closes"
            } else {
                "Middle-click closes"
            });
            if resp.clicked() && !live {
                switch = Some(i);
            }
            if resp.middle_clicked() && (several || unsaved) {
                close = Some(i);
            }
            if live && several {
                // A visible close affordance on the active tab only, to
                // keep the strip quiet.
                let x = ui.add(
                    egui::Button::new(RichText::new("×").color(MUTED))
                        .fill(Color32::TRANSPARENT)
                        .frame(false),
                );
                if x.on_hover_text("Close document").clicked() {
                    close = Some(i);
                }
            }
        }
        let plus = ui.add(
            egui::Button::new(RichText::new("+").color(MUTED))
                .fill(Color32::TRANSPARENT)
                .frame(false),
        );
        if plus.on_hover_text("New document (tab)").clicked() {
            self.dialog = Some(Dialog::New(1920, 1080));
        }
        if let Some(i) = switch {
            self.switch_tab(i);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
    }
}
