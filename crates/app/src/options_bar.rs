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
                        // same fields.
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
                            self.xform = Some(x);
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
                        Tool::Brush | Tool::Eraser | Tool::Clone => {
                            if self.tool == Tool::Brush {
                                for (m, label) in [
                                    (BrushMode::Paint, "Paint"),
                                    (BrushMode::Dodge, "Dodge"),
                                    (BrushMode::Burn, "Burn"),
                                ] {
                                    if ui.selectable_label(self.brush.mode == m, label).clicked() {
                                        self.brush.mode = m;
                                    }
                                }
                                ui.separator();
                            }
                            if self.tool == Tool::Clone {
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
                        Tool::Hand => {
                            ui.label(RichText::new("Drag to pan, scroll to zoom").weak());
                        }
                    }
                });
            });
    }
}
