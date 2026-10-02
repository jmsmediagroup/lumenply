use super::*;

impl App {
    /// One-click non-destructive additions, straight from the concept:
    /// a chip per common adjustment, blur and sharpen, and a menu with the
    /// rest. Everything lands above the active layer as a live layer.
    pub(crate) fn quick_add_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "ADD ABOVE ACTIVE LAYER");
        let mut add_adj: Option<Adjustment> = None;
        let mut add_filter: Option<Filter> = None;
        let chip = |ui: &mut egui::Ui, label: &str| {
            ui.add(
                egui::Button::new(RichText::new(label).size(12.0))
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0, LINE))
                    .min_size(egui::vec2(64.0, 24.0)),
            )
            .clicked()
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (label, name) in [
                ("Curves", "Curves"),
                ("Levels", "Levels"),
                ("Exposure", "Exposure"),
                ("Hue/Sat", "Hue/Saturation"),
                ("B & W", "Black & White"),
            ] {
                if chip(ui, label) {
                    add_adj = adjustment_presets()
                        .into_iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, a)| a);
                }
            }
            if chip(ui, "Blur") {
                add_filter = Some(Filter::GaussianBlur { radius: 8.0 });
            }
            if chip(ui, "Sharpen") {
                add_filter = Some(Filter::Sharpen {
                    amount: 1.0,
                    radius: 2.0,
                });
            }
            egui::menu::menu_custom_button(
                ui,
                egui::Button::new(RichText::new("More...").size(12.0).color(MUTED))
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0, LINE))
                    .min_size(egui::vec2(64.0, 24.0)),
                |ui| {
                    for (name, adj) in adjustment_presets() {
                        if ui.button(name).clicked() {
                            add_adj = Some(adj);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    for (name, f) in filter_presets() {
                        if ui.button(format!("Live {name}")).clicked() {
                            add_filter = Some(f);
                            ui.close_menu();
                        }
                    }
                },
            );
        });
        if let Some(a) = add_adj {
            self.add_adjustment(a);
        }
        if let Some(f) = add_filter {
            self.add_filter_layer(f);
        }
    }

    pub(crate) fn properties_ui(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active else {
            section_title(ui, "PROPERTIES");
            ui.label(RichText::new("No layer selected").weak());
            return;
        };
        let Some(layer) = self.editor.doc().layer(id) else {
            return;
        };
        let name = layer.name.clone();
        // Header: PROPERTIES on the left, a breadcrumb to the edit target
        // ("Hiker › Mask") on the right.
        let on_mask = self.editing_mask && layer.mask.is_some();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("PROPERTIES").small().strong().color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if on_mask {
                    ui.label(RichText::new("Mask").color(ACCENT));
                    ui.label(RichText::new("›").color(MUTED));
                }
                ui.label(RichText::new(&name).color(TEXT));
            });
        });
        let blend = layer.blend;
        let is_group = layer.children().is_some();
        let pass = layer.pass_through;
        let effects = layer.effects.clone();
        let can_fx = !layer.clip
            && matches!(
                layer.content,
                LayerContent::Pixel(_) | LayerContent::Text(_) | LayerContent::Group(_)
            );
        let adj = match &layer.content {
            LayerContent::Adjustment(a) => Some(a.clone()),
            _ => None,
        };
        let filt = match &layer.content {
            LayerContent::Filter(f) => Some(f.clone()),
            _ => None,
        };
        let mut opacity = layer.opacity * 100.0;
        ui.label(RichText::new(name).strong());

        let r = ui
            .horizontal(|ui| {
                ui.add_sized(
                    [70.0, 18.0],
                    egui::Label::new(RichText::new("Opacity").color(MUTED)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_sized(
                        [58.0, 18.0],
                        egui::Label::new(RichText::new(format!("{opacity:.0}%")).monospace().color(TEXT)),
                    );
                    ui.spacing_mut().slider_width = (ui.available_width() - 10.0).max(60.0);
                    ui.add(egui::Slider::new(&mut opacity, 0.0..=100.0).show_value(false))
                })
                .inner
            })
            .inner;
        if r.changed() {
            self.run_coalescing(
                &SetOpacity {
                    layer: id,
                    opacity: opacity / 100.0,
                },
                &format!("opacity-{id}"),
            );
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            self.editor.end_coalescing();
        }

        // Groups offer Pass Through above the regular modes: the children
        // then composite straight onto the backdrop.
        let mut sel = if is_group && pass { None } else { Some(blend) };
        let title = |m: BlendMode| {
            let n = m.name();
            let mut c = n.chars();
            c.next().map_or(String::new(), |f| {
                f.to_uppercase().collect::<String>() + c.as_str()
            })
        };
        ui.horizontal(|ui| {
            ui.add_sized(
                [70.0, 18.0],
                egui::Label::new(RichText::new("Blend").color(MUTED)),
            );
            ui.spacing_mut().combo_width = ui.available_width();
            egui::ComboBox::from_id_salt("blend-mode")
                .selected_text(sel.map_or("Pass Through".into(), title))
                .show_ui(ui, |ui| {
                    if is_group {
                        ui.selectable_value(&mut sel, None, "Pass Through");
                    }
                    for m in BlendMode::ALL {
                        ui.selectable_value(&mut sel, Some(m), title(m));
                    }
                });
        });
        match sel {
            None if !pass => self.run(&SetPassThrough {
                layer: id,
                pass_through: true,
            }),
            Some(m) => {
                if pass {
                    self.run(&SetPassThrough {
                        layer: id,
                        pass_through: false,
                    });
                }
                if m != blend {
                    self.run(&SetBlendMode { layer: id, blend: m });
                }
            }
            _ => {}
        }

        // A text layer leads with its text, above the effects.
        if let Some(t) = self.active_text() {
            self.text_properties(ui, id, t);
        }
        if can_fx {
            self.effects_ui(ui, id, effects);
        }
        if let Some(adj) = adj {
            ui.add_space(4.0);
            ui.label(RichText::new(adj.name()).small().strong());
            self.adjustment_ui(ui, id, adj);
        } else if let Some(mut f) = filt {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("{} (live)", f.name())).small().strong());
            let before = f.clone();
            let mut finished = false;
            match &mut f {
                Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
                    let r = ui.add(
                        egui::Slider::new(radius, 0.5..=60.0)
                            .logarithmic(true)
                            .suffix(" px")
                            .text("Radius"),
                    );
                    finished |= r.drag_stopped() || (r.changed() && !r.dragged());
                }
                Filter::Sharpen { amount, radius } => {
                    finished |= slider_row(ui, "Amount", amount, 0.0..=5.0, "");
                    finished |= slider_row(ui, "Radius", radius, 0.5..=20.0, " px");
                }
                Filter::Noise { amount } => {
                    finished |= slider_row(ui, "Amount", amount, 0.0..=1.0, "");
                }
                Filter::MotionBlur { angle, distance } => {
                    finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
                    finished |= slider_row(ui, "Distance", distance, 1.0..=200.0, " px");
                }
                Filter::Median { radius } => {
                    finished |= slider_row(ui, "Radius", radius, 1.0..=8.0, " px");
                }
                Filter::HighPass { radius } => {
                    finished |= slider_row(ui, "Radius", radius, 0.5..=60.0, " px");
                }
            }
            if f != before {
                self.run_coalescing(&SetFilter { layer: id, filter: f }, &format!("filter-{id}"));
            }
            if finished {
                self.editor.end_coalescing();
            }
            ui.label(
                RichText::new("Applies to everything below; the pixels stay untouched.")
                    .weak()
                    .small(),
            );
        } else if self.active_is_pixel() {
            ui.add_space(4.0);
            ui.label(RichText::new("Transform").small().strong());
            ui.add(
                egui::Slider::new(&mut self.xform_scale, 10.0..=400.0)
                    .suffix("%")
                    .text("Scale"),
            );
            ui.add(
                egui::Slider::new(&mut self.xform_angle, -180.0..=180.0)
                    .suffix("°")
                    .text("Rotate"),
            );
            let mut apply = false;
            let mut flip = None;
            let mut free = false;
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply = true;
                }
                if ui.button("Flip H").clicked() {
                    flip = Some(true);
                }
                if ui.button("Flip V").clicked() {
                    flip = Some(false);
                }
                if ui.button("Free transform").clicked() {
                    free = true;
                }
            });
            if apply {
                let s = self.xform_scale / 100.0;
                let a = self.xform_angle.to_radians();
                if let Some(cmd) = TransformLayer::around_center(self.editor.doc(), id, s, s, a) {
                    self.run(&cmd);
                    self.xform_scale = 100.0;
                    self.xform_angle = 0.0;
                } else {
                    self.status = "Layer has no pixels to transform".into();
                }
            }
            if let Some(h) = flip {
                self.flip_active(h);
            }
            if free {
                self.begin_free_transform();
            }
        }
        self.histogram_footer(ui);
    }

    /// Non-destructive layer effects: toggles and parameters, coalescing
    /// into one history step per drag.
    fn effects_ui(&mut self, ui: &mut egui::Ui, id: LayerId, mut fx: lumenply_doc::LayerEffects) {
        use lumenply_doc::{GlowFx, ShadowFx, StrokeFx};
        section_title(ui, "EFFECTS");
        let mut changed = false;
        let mut finished = false;
        let color_btn = |ui: &mut egui::Ui, c: &mut [f32; 3]| -> bool {
            let mut srgb = c.map(lumenply_io::linear_to_srgb_f);
            let r = crate::color_picker::color_edit_button_rgb(ui, &mut srgb);
            if r.changed() {
                *c = srgb.map(lumenply_io::srgb_to_linear_f);
            }
            r.changed()
        };

        ui.horizontal(|ui| {
            let mut on = fx.drop_shadow.is_some();
            if ui.checkbox(&mut on, "Drop shadow").changed() {
                fx.drop_shadow = on.then(ShadowFx::default);
                changed = true;
                finished = true;
            }
            if let Some(sfx) = &mut fx.drop_shadow {
                let c = color_btn(ui, &mut sfx.color);
                changed |= c;
                finished |= c;
            }
        });
        if let Some(mut sfx) = fx.drop_shadow {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [70.0, 18.0],
                    egui::Label::new(RichText::new("Offset").color(MUTED)),
                );
                let rx = ui.add(egui::DragValue::new(&mut sfx.dx).speed(0.5).range(-200.0..=200.0));
                let ry = ui.add(egui::DragValue::new(&mut sfx.dy).speed(0.5).range(-200.0..=200.0));
                changed |= rx.changed() || ry.changed();
                finished |= rx.drag_stopped() || ry.drag_stopped();
            });
            let f = slider_row(ui, "Blur", &mut sfx.blur, 0.0..=60.0, " px");
            finished |= f;
            let f2 = slider_row(ui, "Opacity", &mut sfx.opacity, 0.0..=1.0, "");
            finished |= f2;
            if sfx != fx.drop_shadow.unwrap() {
                changed = true;
            }
            fx.drop_shadow = Some(sfx);
        }

        ui.horizontal(|ui| {
            let mut on = fx.outer_glow.is_some();
            if ui.checkbox(&mut on, "Outer glow").changed() {
                fx.outer_glow = on.then(GlowFx::default);
                changed = true;
                finished = true;
            }
            if let Some(g) = &mut fx.outer_glow {
                let c = color_btn(ui, &mut g.color);
                changed |= c;
                finished |= c;
            }
        });
        if let Some(mut g) = fx.outer_glow {
            let f = slider_row(ui, "Blur", &mut g.blur, 0.0..=60.0, " px");
            finished |= f;
            let f2 = slider_row(ui, "Opacity", &mut g.opacity, 0.0..=1.0, "");
            finished |= f2;
            if g != fx.outer_glow.unwrap() {
                changed = true;
            }
            fx.outer_glow = Some(g);
        }

        ui.horizontal(|ui| {
            let mut on = fx.inner_shadow.is_some();
            if ui.checkbox(&mut on, "Inner shadow").changed() {
                fx.inner_shadow = on.then(ShadowFx::default);
                changed = true;
                finished = true;
            }
            if let Some(sfx) = &mut fx.inner_shadow {
                let c = color_btn(ui, &mut sfx.color);
                changed |= c;
                finished |= c;
            }
        });
        if let Some(mut sfx) = fx.inner_shadow {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [70.0, 18.0],
                    egui::Label::new(RichText::new("Offset").color(MUTED)),
                );
                let rx = ui.add(egui::DragValue::new(&mut sfx.dx).speed(0.5).range(-200.0..=200.0));
                let ry = ui.add(egui::DragValue::new(&mut sfx.dy).speed(0.5).range(-200.0..=200.0));
                changed |= rx.changed() || ry.changed();
                finished |= rx.drag_stopped() || ry.drag_stopped();
            });
            let f = slider_row(ui, "Blur", &mut sfx.blur, 0.0..=60.0, " px");
            finished |= f;
            let f2 = slider_row(ui, "Opacity", &mut sfx.opacity, 0.0..=1.0, "");
            finished |= f2;
            if sfx != fx.inner_shadow.unwrap() {
                changed = true;
            }
            fx.inner_shadow = Some(sfx);
        }

        ui.horizontal(|ui| {
            let mut on = fx.inner_glow.is_some();
            if ui.checkbox(&mut on, "Inner glow").changed() {
                fx.inner_glow = on.then(GlowFx::default);
                changed = true;
                finished = true;
            }
            if let Some(g) = &mut fx.inner_glow {
                let c = color_btn(ui, &mut g.color);
                changed |= c;
                finished |= c;
            }
        });
        if let Some(mut g) = fx.inner_glow {
            let f = slider_row(ui, "Blur", &mut g.blur, 0.0..=60.0, " px");
            finished |= f;
            let f2 = slider_row(ui, "Opacity", &mut g.opacity, 0.0..=1.0, "");
            finished |= f2;
            if g != fx.inner_glow.unwrap() {
                changed = true;
            }
            fx.inner_glow = Some(g);
        }

        ui.horizontal(|ui| {
            let mut on = fx.bevel.is_some();
            if ui.checkbox(&mut on, "Bevel").changed() {
                fx.bevel = on.then(lumenply_doc::BevelFx::default);
                changed = true;
                finished = true;
            }
            if let Some(b) = &mut fx.bevel {
                let c1 = color_btn(ui, &mut b.highlight);
                let c2 = color_btn(ui, &mut b.shadow);
                changed |= c1 || c2;
                finished |= c1 || c2;
            }
        });
        if let Some(mut b) = fx.bevel {
            let f = slider_row(ui, "Size", &mut b.size, 0.5..=40.0, " px");
            finished |= f;
            let f2 = slider_row(ui, "Depth", &mut b.depth, 0.1..=3.0, "");
            finished |= f2;
            let f3 = slider_row(ui, "Angle", &mut b.angle, 0.0..=360.0, "°");
            finished |= f3;
            let f4 = slider_row(ui, "Opacity", &mut b.opacity, 0.0..=1.0, "");
            finished |= f4;
            if b != fx.bevel.unwrap() {
                changed = true;
            }
            fx.bevel = Some(b);
        }

        ui.horizontal(|ui| {
            let mut on = fx.color_overlay.is_some();
            if ui.checkbox(&mut on, "Color overlay").changed() {
                fx.color_overlay = on.then(lumenply_doc::ColorOverlayFx::default);
                changed = true;
                finished = true;
            }
            if let Some(co) = &mut fx.color_overlay {
                let c = color_btn(ui, &mut co.color);
                changed |= c;
                finished |= c;
            }
        });
        if let Some(mut co) = fx.color_overlay {
            let f = slider_row(ui, "Opacity", &mut co.opacity, 0.0..=1.0, "");
            finished |= f;
            if co != fx.color_overlay.unwrap() {
                changed = true;
            }
            fx.color_overlay = Some(co);
        }

        ui.horizontal(|ui| {
            let mut on = fx.gradient_overlay.is_some();
            if ui.checkbox(&mut on, "Gradient overlay").changed() {
                fx.gradient_overlay = on.then(lumenply_doc::GradientOverlayFx::default);
                changed = true;
                finished = true;
            }
            if let Some(go) = &mut fx.gradient_overlay {
                let c1 = color_btn(ui, &mut go.start);
                let c2 = color_btn(ui, &mut go.end);
                changed |= c1 || c2;
                finished |= c1 || c2;
            }
        });
        if let Some(mut go) = fx.gradient_overlay {
            let f = slider_row(ui, "Angle", &mut go.angle, 0.0..=360.0, "°");
            finished |= f;
            let f2 = slider_row(ui, "Opacity", &mut go.opacity, 0.0..=1.0, "");
            finished |= f2;
            if go != fx.gradient_overlay.unwrap() {
                changed = true;
            }
            fx.gradient_overlay = Some(go);
        }

        ui.horizontal(|ui| {
            let mut on = fx.stroke.is_some();
            if ui.checkbox(&mut on, "Stroke").changed() {
                fx.stroke = on.then(StrokeFx::default);
                changed = true;
                finished = true;
            }
            if let Some(st) = &mut fx.stroke {
                let c = color_btn(ui, &mut st.color);
                changed |= c;
                finished |= c;
            }
        });
        if let Some(mut st) = fx.stroke {
            let f = slider_row(ui, "Size", &mut st.size, 0.5..=40.0, " px");
            finished |= f;
            let f2 = slider_row(ui, "Opacity", &mut st.opacity, 0.0..=1.0, "");
            finished |= f2;
            if st != fx.stroke.unwrap() {
                changed = true;
            }
            fx.stroke = Some(st);
        }

        if changed || finished {
            self.run_coalescing(
                &SetLayerEffects {
                    layer: id,
                    effects: fx,
                },
                &format!("fx-{id}"),
            );
        }
        if finished && !crate::color_picker::is_dragging(ui.ctx()) {
            self.editor.end_coalescing();
        }
    }

    fn histogram_footer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        self.histogram_ui(ui);
    }

    pub(crate) fn adjustment_ui(&mut self, ui: &mut egui::Ui, id: LayerId, mut adj: Adjustment) {
        let before = adj.clone();
        let mut finished = false;
        match &mut adj {
            Adjustment::Invert => {
                ui.label(RichText::new("This adjustment has no settings.").weak());
            }
            Adjustment::BrightnessContrast { brightness, contrast } => {
                finished |= slider_row(ui, "Brightness", brightness, -1.0..=1.0, "");
                finished |= slider_row(ui, "Contrast", contrast, -1.0..=1.0, "");
            }
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                finished |= slider_row(ui, "Hue", hue, -180.0..=180.0, "°");
                finished |= slider_row(ui, "Saturation", saturation, -1.0..=1.0, "");
                finished |= slider_row(ui, "Lightness", lightness, -1.0..=1.0, "");
            }
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                channels,
            } => {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.levels_ch, 0, "Master");
                    ui.selectable_value(&mut self.levels_ch, 1, "R");
                    ui.selectable_value(&mut self.levels_ch, 2, "G");
                    ui.selectable_value(&mut self.levels_ch, 3, "B");
                });
                let (ib, iw, g, ob, ow) = match self.levels_ch {
                    0 => (in_black, in_white, gamma, out_black, out_white),
                    n => {
                        let c = &mut channels[n - 1];
                        (
                            &mut c.in_black,
                            &mut c.in_white,
                            &mut c.gamma,
                            &mut c.out_black,
                            &mut c.out_white,
                        )
                    }
                };
                finished |= slider_row(ui, "Input black", ib, 0.0..=1.0, "");
                finished |= slider_row(ui, "Input white", iw, 0.0..=1.0, "");
                finished |= slider_row(ui, "Gamma", g, 0.1..=4.0, "");
                finished |= slider_row(ui, "Output black", ob, 0.0..=1.0, "");
                finished |= slider_row(ui, "Output white", ow, 0.0..=1.0, "");
            }
            Adjustment::Curves { points } => {
                finished |= curve_editor(ui, points, &mut self.curve_drag);
            }
            Adjustment::BlackWhite { red, green, blue } => {
                finished |= slider_row(ui, "Red", red, 0.0..=1.0, "");
                finished |= slider_row(ui, "Green", green, 0.0..=1.0, "");
                finished |= slider_row(ui, "Blue", blue, 0.0..=1.0, "");
            }
            Adjustment::Exposure {
                exposure,
                offset,
                gamma,
            } => {
                finished |= slider_row(ui, "Exposure", exposure, -4.0..=4.0, " EV");
                finished |= slider_row(ui, "Offset", offset, -0.5..=0.5, "");
                finished |= slider_row(ui, "Gamma", gamma, 0.1..=4.0, "");
            }
            Adjustment::ColorBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.cb_tone, 0, "Shadows");
                    ui.selectable_value(&mut self.cb_tone, 1, "Midtones");
                    ui.selectable_value(&mut self.cb_tone, 2, "Highlights");
                });
                let tone = match self.cb_tone {
                    0 => shadows,
                    1 => midtones,
                    _ => highlights,
                };
                finished |= slider_row(ui, "Cyan ↔ Red", &mut tone[0], -1.0..=1.0, "");
                finished |= slider_row(ui, "Magenta ↔ Green", &mut tone[1], -1.0..=1.0, "");
                finished |= slider_row(ui, "Yellow ↔ Blue", &mut tone[2], -1.0..=1.0, "");
                if ui.checkbox(preserve_luminosity, "Preserve luminosity").changed() {
                    finished = true;
                }
            }
            Adjustment::Vibrance { vibrance, saturation } => {
                finished |= slider_row(ui, "Vibrance", vibrance, -1.0..=1.0, "");
                finished |= slider_row(ui, "Saturation", saturation, -1.0..=1.0, "");
            }
            Adjustment::Threshold { level } => {
                finished |= slider_row(ui, "Level", level, 0.0..=1.0, "");
            }
            Adjustment::Posterize { levels } => {
                let mut v = *levels as f32;
                let r = ui.add(egui::Slider::new(&mut v, 2.0..=32.0).integer().text("Levels"));
                *levels = v.round() as u32;
                finished |= r.drag_stopped() || (r.changed() && !r.dragged());
            }
        }
        if adj != before {
            self.run_coalescing(
                &SetAdjustment {
                    layer: id,
                    adjustment: adj,
                },
                &format!("adj-{id}"),
            );
        }
        if finished {
            self.editor.end_coalescing();
        }
    }
}

/// An interactive curve: drag points, click to add, right-click to remove.
pub(crate) fn curve_editor(ui: &mut egui::Ui, points: &mut Vec<[f32; 2]>, drag: &mut Option<usize>) -> bool {
    let size = Vec2::splat(210.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, GROUND);
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let gx = rect.min.x + t * rect.width();
        let gy = rect.min.y + t * rect.height();
        let grid = Stroke::new(1.0, LINE);
        p.line_segment([egui::pos2(gx, rect.min.y), egui::pos2(gx, rect.max.y)], grid);
        p.line_segment([egui::pos2(rect.min.x, gy), egui::pos2(rect.max.x, gy)], grid);
    }
    let to_screen =
        |x: f32, y: f32| egui::pos2(rect.min.x + x * rect.width(), rect.max.y - y * rect.height());
    let from_screen = |q: Pos2| {
        (
            ((q.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
            ((rect.max.y - q.y) / rect.height()).clamp(0.0, 1.0),
        )
    };
    p.line_segment(
        [to_screen(0.0, 0.0), to_screen(1.0, 1.0)],
        Stroke::new(1.0, MUTED),
    );
    let nearest = |pts: &[[f32; 2]], q: Pos2| -> Option<usize> {
        pts.iter()
            .enumerate()
            .map(|(i, pt)| (i, to_screen(pt[0], pt[1]).distance(q)))
            .filter(|(_, d)| *d <= 14.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut finished = false;
    if resp.drag_started() {
        if let Some(q) = ui.input(|i| i.pointer.press_origin()) {
            *drag = nearest(points, q);
        }
    }
    if resp.dragged() {
        if let (Some(i), Some(q)) = (*drag, resp.interact_pointer_pos()) {
            let (x, y) = from_screen(q);
            let last = points.len() - 1;
            let x = if i == 0 || i == last {
                points[i][0]
            } else {
                x.clamp(points[i - 1][0] + 0.01, points[i + 1][0] - 0.01)
            };
            points[i] = [x, y];
        }
    }
    if resp.drag_stopped() {
        *drag = None;
        finished = true;
    }
    if resp.clicked() {
        if let Some(q) = resp.interact_pointer_pos() {
            if nearest(points, q).is_none() && points.len() < 16 {
                let (x, y) = from_screen(q);
                let at = points.iter().position(|pt| pt[0] > x).unwrap_or(points.len());
                if at > 0 && at < points.len() {
                    points.insert(at, [x, y]);
                }
                finished = true;
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(q) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(points, q) {
                if i != 0 && i != points.len() - 1 && points.len() > 2 {
                    points.remove(i);
                    finished = true;
                }
            }
        }
    }
    let compiled = Adjustment::Curves {
        points: points.clone(),
    }
    .compile();
    let line: Vec<Pos2> = (0..=64)
        .map(|i| {
            let x = i as f32 / 64.0;
            to_screen(x, compiled.apply([x; 3])[0])
        })
        .collect();
    p.add(Shape::line(
        line,
        Stroke::new(2.0, Color32::from_rgb(140, 185, 255)),
    ));
    for pt in points.iter() {
        let c = to_screen(pt[0], pt[1]);
        p.circle_filled(c, 5.0, Color32::WHITE);
        p.circle_stroke(c, 5.0, Stroke::new(1.5, ACCENT));
    }
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, MUTED));
    ui.horizontal(|ui| {
        let mut preset = None;
        if ui.small_button("Linear").clicked() {
            preset = Some(vec![[0.0, 0.0], [1.0, 1.0]]);
        }
        if ui.small_button("Contrast").clicked() {
            preset = Some(vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]);
        }
        if ui.small_button("Lighten").clicked() {
            preset = Some(vec![[0.0, 0.0], [0.5, 0.65], [1.0, 1.0]]);
        }
        if ui.small_button("Fade").clicked() {
            preset = Some(vec![[0.0, 0.12], [1.0, 0.9]]);
        }
        if let Some(pts) = preset {
            *points = pts;
            finished = true;
        }
    });
    finished
}
