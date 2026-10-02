use super::*;

impl App {
    // ---- options bar ----------------------------------------------------------------

    pub(crate) fn options_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("options")
            .exact_height(42.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().slider_width = 110.0;
                    if let Some(mut x) = self.xform.clone() {
                        ui.label(RichText::new("Free Transform").strong());
                        ui.separator();
                        // Editable numbers; the canvas handles drive the
                        // same fields. In perspective mode the corners are
                        // free points, so the numbers go quiet.
                        let persp = x.quad.is_some();
                        let warping = x.warp.is_some();
                        // Smart objects re-render through an affine only.
                        let smart = self
                            .editor
                            .doc()
                            .layer(x.layer)
                            .is_some_and(|l| l.smart_layer().is_some());
                        let affine_only = "Smart objects keep affine transforms; rasterize first";
                        ui.add_enabled_ui(!persp && !warping, |ui| {
                            let mut sx = x.sx * 100.0;
                            let mut sy = x.sy * 100.0;
                            let mut rot = x.angle.to_degrees();
                            let mut skew = x.shear.atan().to_degrees();
                            ui.label(RichText::new("W").color(MUTED));
                            let c1 = ui
                                .add(egui::DragValue::new(&mut sx).speed(1.0).suffix("%"))
                                .changed();
                            ui.label(RichText::new("H").color(MUTED));
                            let c2 = ui
                                .add(egui::DragValue::new(&mut sy).speed(1.0).suffix("%"))
                                .changed();
                            ui.label(RichText::new("Rotate").color(MUTED));
                            let c3 = ui
                                .add(egui::DragValue::new(&mut rot).speed(0.5).suffix("°"))
                                .changed();
                            ui.label(RichText::new("Skew").color(MUTED));
                            let c4 = ui
                                .add(
                                    egui::DragValue::new(&mut skew)
                                        .speed(0.5)
                                        .range(-80.0..=80.0)
                                        .suffix("°"),
                                )
                                .changed();
                            if c1 || c2 || c3 || c4 {
                                x.sx = (sx / 100.0).clamp(-50.0, 50.0);
                                x.sy = (sy / 100.0).clamp(-50.0, 50.0);
                                x.angle = rot.to_radians();
                                x.shear = skew.to_radians().tan();
                                self.preview_xform(ctx, &mut x);
                                self.xform = Some(x.clone());
                            }
                        });
                        let mut persp = persp;
                        if ui
                            .add_enabled(!smart, egui::Checkbox::new(&mut persp, "Perspective"))
                            .on_hover_text("Drag each corner freely; edges carry both of their corners")
                            .on_disabled_hover_text(affine_only)
                            .changed()
                        {
                            x.quad = persp.then(|| x.corners());
                            x.warp = None;
                            self.preview_xform(ctx, &mut x);
                            self.xform = Some(x.clone());
                        }
                        let mut warping = warping;
                        if ui
                            .add_enabled(!smart, egui::Checkbox::new(&mut warping, "Warp"))
                            .on_hover_text("Bend the layer through a 4×4 mesh: drag any point")
                            .on_disabled_hover_text(affine_only)
                            .changed()
                        {
                            // Start the mesh where the box is now, so
                            // toggling on changes nothing until a drag.
                            x.warp = warping.then(|| x.initial_warp());
                            x.quad = None;
                            self.preview_xform(ctx, &mut x);
                            self.xform = Some(x.clone());
                        }
                        ui.separator();
                        if ui.button("Apply   Enter").clicked() {
                            self.commit_free_transform();
                        }
                        if ui.button("Cancel   Esc").clicked() {
                            self.cancel_free_transform();
                        }
                        return;
                    }
                    ui.label(
                        RichText::new(self.tool.name())
                            .family(egui::FontFamily::Name("semibold".into()))
                            .color(TEXT),
                    );
                    if self.quick_mask {
                        let red = Color32::from_rgb(0xE8, 0x5D, 0x5D);
                        let chip = RichText::new("QUICK MASK").small().color(red);
                        ui.add(
                            egui::Button::new(chip)
                                .fill(PANEL)
                                .stroke(Stroke::new(1.0, red))
                                .sense(Sense::hover()),
                        )
                        .on_hover_text("Painting edits the selection (Q exits)");
                    }
                    if self.editing_mask {
                        let chip = RichText::new("ON MASK").small().color(ACCENT);
                        ui.add(
                            egui::Button::new(chip)
                                .fill(PANEL)
                                .stroke(Stroke::new(1.0, ACCENT))
                                .sense(Sense::hover()),
                        )
                        .on_hover_text("Edits paint on the layer mask (white reveals, black hides)");
                    }
                    ui.separator();
                    match self.tool {
                        Tool::Move => {
                            ui.label(RichText::new("Drag to move the active layer").weak());
                            if ui
                                .add_enabled(self.active_is_pixel(), egui::Button::new("Free transform"))
                                .clicked()
                            {
                                self.begin_free_transform();
                            }
                        }
                        Tool::Eyedropper => {
                            ui.label(RichText::new("Click to pick the brush colour from the image").weak());
                        }
                        Tool::Bucket | Tool::Wand => {
                            let mut tol = self.tolerance * 100.0;
                            ui.label("Tolerance");
                            if ui
                                .add(egui::Slider::new(&mut tol, 0.0..=100.0).suffix("%"))
                                .changed()
                            {
                                self.tolerance = tol / 100.0;
                            }
                            ui.checkbox(&mut self.contiguous, "Contiguous");
                            ui.checkbox(&mut self.sample_merged, "Sample all layers");
                            if self.tool == Tool::Bucket {
                                let mut op = self.brush.color[3] * 100.0;
                                ui.label("Opacity");
                                if ui
                                    .add(egui::Slider::new(&mut op, 1.0..=100.0).suffix("%"))
                                    .changed()
                                {
                                    self.brush.color[3] = op / 100.0;
                                }
                            } else {
                                ui.separator();
                                ui.selectable_value(&mut self.select_op, CombineOp::Replace, "New");
                                ui.selectable_value(&mut self.select_op, CombineOp::Union, "Add");
                                ui.selectable_value(&mut self.select_op, CombineOp::Subtract, "Subtract");
                                ui.selectable_value(&mut self.select_op, CombineOp::Intersect, "Intersect");
                            }
                        }
                        Tool::Text => {
                            if let (Some(id), Some(t)) = (self.active, self.active_text()) {
                                self.text_controls(ui, id, t, false);
                                if ui.button("Rasterize").clicked() {
                                    self.run(&RasterizeLayer { layer: id });
                                }
                            } else {
                                ui.add(
                                    egui::Slider::new(&mut self.text_size, 6.0..=400.0)
                                        .logarithmic(true)
                                        .suffix(" px")
                                        .text("Size"),
                                );
                                ui.checkbox(&mut self.text_bold, "Bold");
                                ui.checkbox(&mut self.text_italic, "Italic");
                                let mut font = std::mem::take(&mut self.text_font);
                                crate::font_picker(ui, &mut font);
                                self.text_font = font;
                                ui.label(RichText::new("Click on the canvas to add text").weak());
                            }
                        }
                        Tool::Gradient => {
                            ui.selectable_value(&mut self.gradient_kind, GradientKind::Linear, "Linear");
                            ui.selectable_value(&mut self.gradient_kind, GradientKind::Radial, "Radial");
                            ui.separator();
                            ui.label("From");
                            egui::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                            ui.label("To");
                            egui::color_picker::color_edit_button_rgb(ui, &mut self.bg_rgb);
                            ui.checkbox(&mut self.gradient_to_transparent, "To transparent");
                            ui.label(RichText::new("Drag on the canvas").weak());
                        }
                        Tool::Brush | Tool::Eraser | Tool::Clone | Tool::Heal => {
                            if self.tool == Tool::Brush {
                                for (m, label) in [
                                    (BrushMode::Paint, "Paint"),
                                    (BrushMode::Dodge, "Dodge"),
                                    (BrushMode::Burn, "Burn"),
                                    (BrushMode::Smudge, "Smudge"),
                                    (BrushMode::Saturate, "Sat+"),
                                    (BrushMode::Desaturate, "Sat−"),
                                ] {
                                    if ui.selectable_label(self.brush.mode == m, label).clicked() {
                                        self.brush.mode = m;
                                    }
                                }
                                ui.separator();
                                self.brush_presets_ui(ui);
                                ui.separator();
                            }
                            if self.tool == Tool::Heal {
                                ui.checkbox(&mut self.heal_spot, "Spot").on_hover_text(
                                    "Heal from the surroundings alone; untick to add texture from a picked source",
                                );
                            }
                            let needs_source = self.tool == Tool::Clone
                                || (self.tool == Tool::Heal && !self.heal_spot);
                            if needs_source {
                                let picking = self.clone_picking || self.clone_source.is_none();
                                if ui
                                    .selectable_label(picking, "Pick source")
                                    .on_hover_text("Next click sets the clone source (or Alt+click)")
                                    .clicked()
                                {
                                    self.clone_picking = true;
                                }
                                match self.clone_source {
                                    Some((x, y)) => ui.label(format!("Source {:.0}, {:.0}", x, y)),
                                    None => ui.label(RichText::new("No source yet").weak()),
                                };
                                ui.checkbox(&mut self.sample_merged, "Sample all layers");
                                ui.separator();
                            }
                            let mut size = self.brush.radius * 2.0;
                            ui.label("Size");
                            if ui
                                .add(
                                    egui::Slider::new(&mut size, 2.0..=400.0)
                                        .logarithmic(true)
                                        .suffix(" px"),
                                )
                                .changed()
                            {
                                self.brush.radius = size / 2.0;
                            }
                            let mut hard = self.brush.hardness * 100.0;
                            ui.label("Hardness");
                            if ui
                                .add(egui::Slider::new(&mut hard, 0.0..=100.0).suffix("%"))
                                .changed()
                            {
                                self.brush.hardness = hard / 100.0;
                            }
                            let mut op = self.brush.color[3] * 100.0;
                            ui.label("Opacity");
                            if ui
                                .add(egui::Slider::new(&mut op, 1.0..=100.0).suffix("%"))
                                .changed()
                            {
                                self.brush.color[3] = op / 100.0;
                            }
                            let mut sc = self.brush.jitter * 100.0;
                            ui.label("Scatter");
                            if ui
                                .add(egui::Slider::new(&mut sc, 0.0..=100.0).suffix("%"))
                                .changed()
                            {
                                self.brush.jitter = sc / 100.0;
                            }
                            if self.tool == Tool::Brush && self.editing_mask {
                                if ui.small_button("White").clicked() {
                                    self.brush_rgb = [1.0; 3];
                                }
                                if ui.small_button("Black").clicked() {
                                    self.brush_rgb = [0.0; 3];
                                }
                            }
                        }
                        Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::PolyLasso => {
                            if self.tool == Tool::PolyLasso {
                                ui.label(RichText::new("Click to add points, double-click to close").weak());
                                if !self.lasso.is_empty() {
                                    if ui.button("Close").clicked() {
                                        self.finish_polygon(ctx);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        self.lasso.clear();
                                    }
                                }
                                ui.separator();
                            }
                            ui.selectable_value(&mut self.select_op, CombineOp::Replace, "New");
                            ui.selectable_value(&mut self.select_op, CombineOp::Union, "Add");
                            ui.selectable_value(&mut self.select_op, CombineOp::Subtract, "Subtract");
                            ui.selectable_value(&mut self.select_op, CombineOp::Intersect, "Intersect");
                            ui.separator();
                            ui.label("Feather");
                            ui.add(egui::Slider::new(&mut self.feather, 0.0..=100.0).suffix(" px"));
                            let has_sel = self.editor.doc().selection.is_some();
                            if ui.add_enabled(has_sel, egui::Button::new("Apply")).clicked() {
                                self.run(&FeatherSelection { radius: self.feather });
                            }
                            ui.label(RichText::new("Shift adds, Alt subtracts").weak());
                        }
                        Tool::Pen => {
                            let has_path = self.editor.doc().work_path.is_some();
                            ui.label(
                                RichText::new("Click corners, drag curves; click the first point to close")
                                    .weak(),
                            );
                            ui.separator();
                            if ui
                                .add_enabled(
                                    has_path && self.active_is_pixel(),
                                    egui::Button::new("Fill path"),
                                )
                                .clicked()
                            {
                                if let Some(layer) = self.active {
                                    let color = self.make_brush().color;
                                    self.run(&FillPath { layer, color });
                                }
                            }
                            if ui
                                .add_enabled(
                                    has_path && self.active_is_pixel(),
                                    egui::Button::new("Stroke path"),
                                )
                                .clicked()
                            {
                                if let Some(layer) = self.active {
                                    let mut brush = self.make_brush();
                                    brush.mode = BrushMode::Paint;
                                    self.run(&StrokeWorkPath { layer, brush });
                                }
                            }
                            if ui
                                .add_enabled(has_path, egui::Button::new("Make selection"))
                                .clicked()
                            {
                                self.run(&PathToSelection {
                                    op: self.select_op,
                                });
                            }
                            if ui
                                .add_enabled(has_path, egui::Button::new("Clear path"))
                                .clicked()
                            {
                                self.pen_open = false;
                                self.run(&SetWorkPath { path: None });
                            }
                            ui.separator();
                            if ui
                                .add_enabled(has_path, egui::Button::new("Save path"))
                                .on_hover_text("Keep a named copy in the document's Paths list")
                                .clicked()
                            {
                                let name = format!("Path {}", self.editor.doc().saved_paths.len() + 1);
                                self.run(&SaveWorkPath { name });
                            }
                            let names: Vec<String> = self
                                .editor
                                .doc()
                                .saved_paths
                                .iter()
                                .map(|n| n.name.clone())
                                .collect();
                            if !names.is_empty() {
                                let mut load = None;
                                let mut delete = None;
                                egui::ComboBox::from_id_salt("saved-paths")
                                    .selected_text(format!("Paths ({})", names.len()))
                                    .width(130.0)
                                    .show_ui(ui, |ui| {
                                        for (i, name) in names.iter().enumerate() {
                                            ui.horizontal(|ui| {
                                                if ui.selectable_label(false, name).clicked() {
                                                    load = Some(i);
                                                }
                                                if ui
                                                    .small_button("✕")
                                                    .on_hover_text("Delete this saved path")
                                                    .clicked()
                                                {
                                                    delete = Some(i);
                                                }
                                            });
                                        }
                                    });
                                if let Some(i) = load {
                                    self.pen_open = false;
                                    self.pen_sel = None;
                                    self.run(&UseSavedPath { index: i });
                                }
                                if let Some(i) = delete {
                                    self.run(&DeleteSavedPath { index: i });
                                }
                            }
                        }
                        Tool::Hand => {
                            ui.label(RichText::new("Drag to pan, scroll to zoom").weak());
                        }
                    }
                });
            });
    }

    /// Preset dropdown + save button for the brush's shape parameters.
    fn brush_presets_ui(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Save preset")
            .on_hover_text("Remember the current size, hardness, opacity, spacing and scatter")
            .clicked()
        {
            let name = format!(
                "{} {:.0}",
                if self.brush.hardness >= 0.5 {
                    "Hard"
                } else {
                    "Soft"
                },
                self.brush.radius * 2.0
            );
            self.prefs.brush_presets.push(session::BrushPreset {
                name,
                radius: self.brush.radius,
                hardness: self.brush.hardness,
                spacing: self.brush.spacing,
                jitter: self.brush.jitter,
                opacity: self.brush.color[3],
            });
            self.prefs.save();
            self.status = "Brush preset saved".into();
        }
        if self.prefs.brush_presets.is_empty() {
            return;
        }
        let mut apply = None;
        let mut delete = None;
        egui::ComboBox::from_id_salt("brush-presets")
            .selected_text(format!("Presets ({})", self.prefs.brush_presets.len()))
            .width(120.0)
            .show_ui(ui, |ui| {
                for (i, p) in self.prefs.brush_presets.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(false, &p.name).clicked() {
                            apply = Some(i);
                        }
                        if ui.small_button("✕").on_hover_text("Delete this preset").clicked() {
                            delete = Some(i);
                        }
                    });
                }
            });
        if let Some(i) = apply {
            let p = self.prefs.brush_presets[i].clone();
            self.brush.radius = p.radius.clamp(0.5, 500.0);
            self.brush.hardness = p.hardness.clamp(0.0, 1.0);
            self.brush.spacing = p.spacing.clamp(0.02, 2.0);
            self.brush.jitter = p.jitter.clamp(0.0, 1.0);
            self.brush.color[3] = p.opacity.clamp(0.0, 1.0);
            self.status = format!("Brush preset \"{}\" applied", p.name);
        }
        if let Some(i) = delete {
            self.prefs.brush_presets.remove(i);
            self.prefs.save();
        }
    }
}
