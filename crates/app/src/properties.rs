use super::*;

impl App {
    /// One-click non-destructive additions, straight from the concept:
    /// a chip per common adjustment, blur and sharpen, and a menu with the
    /// rest. Everything lands above the active layer as a live layer.
    pub(crate) fn quick_add_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "ADD ABOVE ACTIVE LAYER");
        let mut add_adj: Option<Adjustment> = None;
        let mut add_filter: Option<Filter> = None;
        let mut add_fill: Option<bool> = None;
        // Raised by the dock's `raise_controls`, so hover and press show.
        let chip = |ui: &mut egui::Ui, label: &str, tip: String| {
            ui.add(egui::Button::new(RichText::new(label).size(12.0)).min_size(egui::vec2(64.0, 24.0)))
                .on_hover_text(tip)
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
                if chip(
                    ui,
                    label,
                    format!("New {name} adjustment layer above the active layer"),
                ) {
                    add_adj = adjustment_presets()
                        .into_iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, a)| a);
                }
            }
            if chip(
                ui,
                "Blur",
                "New live Gaussian Blur layer above the active layer".into(),
            ) {
                add_filter = Some(Filter::GaussianBlur { radius: 8.0 });
            }
            if chip(
                ui,
                "Sharpen",
                "New live Sharpen layer above the active layer".into(),
            ) {
                add_filter = Some(Filter::Sharpen {
                    amount: 1.0,
                    radius: 2.0,
                });
            }
            menu_custom_button(
                ui,
                egui::Button::new(RichText::new("More…").size(12.0).color(MUTED))
                    .min_size(egui::vec2(64.0, 24.0)),
                |ui| {
                    // Two columns keep this menu short enough to open below
                    // the chips instead of covering the bars above.
                    ui.horizontal_top(|ui| {
                        // Fifteen adjustments: two columns of them.
                        let adjs = adjustment_presets();
                        let half = adjs.len().div_ceil(2);
                        for (col, chunk) in adjs.chunks(half).enumerate() {
                            ui.vertical(|ui| {
                                menu_heading(ui, if col == 0 { "ADJUSTMENT" } else { " " });
                                for (name, adj) in chunk {
                                    if menu_item(ui, name, "") {
                                        add_adj = Some(adj.clone());
                                        ui.close_menu();
                                    }
                                }
                            });
                            ui.add_space(6.0);
                        }
                        ui.vertical(|ui| {
                            menu_heading(ui, "LIVE FILTER");
                            for (name, f) in filter_presets() {
                                if menu_item(ui, name, "") {
                                    add_filter = Some(f);
                                    ui.close_menu();
                                }
                            }
                            menu_heading(ui, "FILL LAYER");
                            for (name, gradient) in [("Solid color", false), ("Gradient", true)] {
                                if menu_item(ui, name, "") {
                                    add_fill = Some(gradient);
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                },
            );
        });
        if let Some(a) = add_adj {
            self.add_adjustment(a);
        }
        if let Some(f) = add_filter {
            self.add_filter_layer(f);
        }
        if let Some(gradient) = add_fill {
            self.add_fill_layer(gradient);
        }
    }

    pub(crate) fn properties_ui(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active else {
            section_title(ui, "PROPERTIES");
            ui.add_space(2.0);
            let hint = if self.editor.doc().layer_count() == 0 {
                "Nothing to edit yet: add a layer below."
            } else {
                "Select a layer to see its settings."
            };
            ui.label(RichText::new(hint).color(MUTED));
            return;
        };
        let Some(layer) = self.editor.doc().layer(id) else {
            return;
        };
        let name = layer.name.clone();
        // Header: PROPERTIES on the left, a breadcrumb to the edit target
        // ("Hiker › Mask") on the right; a long name is elided, never
        // allowed to run into the title or widen the dock.
        let on_mask = self.editing_mask && layer.mask.is_some();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("PROPERTIES").small().strong().color(MUTED));
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if on_mask {
                    ui.label(RichText::new("Mask").color(ACCENT));
                    ui.label(RichText::new("›").color(MUTED));
                }
                ui.add(egui::Label::new(RichText::new(&name).color(TEXT)).truncate());
            });
        });
        let blend = layer.blend;
        let is_group = layer.children().is_some();
        let pass = layer.pass_through;
        let effects = layer.effects.clone();
        let can_fx = !layer.clip
            && matches!(
                layer.content,
                LayerContent::Pixel(_)
                    | LayerContent::Text(_)
                    | LayerContent::Smart(_)
                    | LayerContent::Group(_)
                    | LayerContent::Fill(_)
                    | LayerContent::Shape(_)
            );
        let is_smart = layer.smart_layer().is_some();
        let fill = layer.fill_layer().map(|f| f.fill.clone());
        let shape = layer.shape_layer().cloned();
        // The painted bounds' centre: transforms pivot about it.
        let pivot = layer
            .raster_store()
            .and_then(|s| s.content_bounds())
            .map(|b| (b.x as f32 + b.w as f32 / 2.0, b.y as f32 + b.h as f32 / 2.0));
        let adj = match &layer.content {
            LayerContent::Adjustment(a) => Some(a.clone()),
            _ => None,
        };
        let filt = match &layer.content {
            LayerContent::Filter(f) => Some(f.clone()),
            _ => None,
        };
        let mut opacity = layer.opacity * 100.0;
        let before = opacity;
        // Photoshop's Fill: the layer's own content, not its effects.
        let has_fill = matches!(
            layer.content,
            LayerContent::Pixel(_)
                | LayerContent::Text(_)
                | LayerContent::Smart(_)
                | LayerContent::Group(_)
                | LayerContent::Fill(_)
                | LayerContent::Shape(_)
        );
        let mut fill_pct = layer.fill_opacity * 100.0;
        let fill_before = fill_pct;
        ui.add_space(2.0);
        let finished = slider_row(ui, "Opacity", &mut opacity, 0.0..=100.0, "%");
        if opacity != before {
            self.run_coalescing(
                &SetOpacity {
                    layer: id,
                    opacity: opacity / 100.0,
                },
                &format!("opacity-{id}"),
            );
        }
        if finished {
            self.editor.end_coalescing();
        }
        if has_fill {
            let finished = slider_row(ui, "Fill", &mut fill_pct, 0.0..=100.0, "%");
            if fill_pct != fill_before {
                self.run_coalescing(
                    &lumenply_core::fill_opacity::SetFillOpacity {
                        layer: id,
                        fill: fill_pct / 100.0,
                    },
                    &format!("fill-opacity-{id}"),
                );
            }
            if finished {
                self.editor.end_coalescing();
            }
        }

        // Groups offer Pass Through above the regular modes: the children
        // then composite straight onto the backdrop.
        let mut sel = if is_group && pass { None } else { Some(blend) };
        ui.horizontal(|ui| {
            row_label(ui, "Blend", LABEL_W);
            ui.spacing_mut().combo_width = ui.available_width();
            crate::blend_ui::blend_combo(ui, "blend-mode", "Blend mode", None, &mut sel, is_group);
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

        // Smart filters on this layer (smart_filters_ui), right under its
        // blend: once added they are what gets tuned most.
        self.smart_filters_properties(ui, id);

        // What this kind of layer does comes first; the (long, mostly
        // unused) effects list folds away below it.
        if let Some(t) = self.active_text() {
            self.text_properties(ui, id, t);
        } else if let Some(adj) = adj {
            section_title(ui, &adj.name().to_uppercase());
            self.adjustment_ui(ui, id, adj);
        } else if let Some(sh) = shape {
            section_title(ui, &format!("SHAPE · {}", sh.geometry.name().to_uppercase()));
            self.shape_properties(ui, id, sh);
        } else if let Some(f) = fill {
            section_title(ui, &f.name().to_uppercase());
            self.fill_ui(ui, id, f);
        } else if let Some(mut f) = filt {
            section_title(ui, &format!("{} · LIVE", f.name().to_uppercase()));
            let before = f.clone();
            let mut finished = false;
            match &mut f {
                Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
                    finished |= slider_row_log(ui, "Radius", radius, 0.5..=60.0, " px");
                }
                Filter::Sharpen { amount, radius } => {
                    finished |= slider_row_scaled(ui, "Amount", amount, 0.0..=5.0, 100.0, "%");
                    finished |= slider_row(ui, "Radius", radius, 0.5..=20.0, " px");
                }
                Filter::Noise { amount } => {
                    finished |= slider_row_scaled(ui, "Amount", amount, 0.0..=1.0, 100.0, "%");
                }
                Filter::MotionBlur { angle, distance } => {
                    finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
                    finished |= slider_row(ui, "Distance", distance, 1.0..=200.0, " px");
                }
                Filter::Median { radius } => {
                    let o = RowOpts {
                        int: true,
                        ..RowOpts::default()
                    };
                    finished |= slider_row_ex(ui, "Radius", radius, 1.0..=8.0, " px", o);
                }
                Filter::HighPass { radius } => {
                    finished |= slider_row(ui, "Radius", radius, 0.5..=60.0, " px");
                }
                Filter::Mosaic { size } => {
                    let o = RowOpts {
                        int: true,
                        log: true,
                        ..RowOpts::default()
                    };
                    finished |= slider_row_ex(ui, "Cell size", size, 2.0..=200.0, " px", o);
                }
                Filter::Emboss {
                    angle,
                    height,
                    amount,
                } => {
                    finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
                    finished |= slider_row(ui, "Height", height, 1.0..=10.0, " px");
                    finished |= slider_row_scaled(ui, "Amount", amount, 0.0..=5.0, 100.0, "%");
                }
                Filter::FindEdges => {}
                Filter::SurfaceBlur { radius, threshold } => {
                    let int = RowOpts {
                        int: true,
                        ..RowOpts::default()
                    };
                    let o = RowOpts { log: true, ..int };
                    finished |= slider_row_ex(ui, "Radius", radius, 1.0..=100.0, " px", o);
                    finished |= slider_row_ex(ui, "Threshold", threshold, 2.0..=255.0, " levels", int);
                }
                Filter::LensBlur { radius, highlights } => {
                    finished |= slider_row_log(ui, "Radius", radius, 1.0..=100.0, " px");
                    finished |= slider_row_scaled(ui, "Highlights", highlights, 0.0..=1.0, 100.0, "%");
                }
                Filter::DustScratches { radius, threshold } => {
                    let o = RowOpts {
                        int: true,
                        ..RowOpts::default()
                    };
                    finished |= slider_row_ex(ui, "Radius", radius, 1.0..=8.0, " px", o);
                    finished |= slider_row_ex(ui, "Threshold", threshold, 0.0..=255.0, " levels", o);
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
                    .small()
                    .color(MUTED),
            );
        } else if self.active_is_pixel() || is_smart {
            let mut rasterize = false;
            let mut contents: Option<&'static str> = None;
            if is_smart {
                ui.horizontal(|ui| {
                    section_title(ui, "SMART OBJECT");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        rasterize = ui
                            .add(
                                egui::Button::new(RichText::new("Rasterize").small().color(MUTED))
                                    .frame(false),
                            )
                            .on_hover_text("Turn it into ordinary pixels (transforms resample from then on)")
                            .clicked();
                    });
                });
                ui.label(
                    RichText::new(
                        "Scale and rotate as often as you like: it re-renders from the original pixels.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("Edit contents")
                        .on_hover_text("Open the original pixels in a tab; Save there updates this layer")
                        .clicked()
                    {
                        contents = Some("smart-edit");
                    }
                    if ui
                        .button("Replace contents…")
                        .on_hover_text("Put another image in, keeping the transform")
                        .clicked()
                    {
                        contents = Some("smart-replace");
                    }
                });
            } else {
                section_title(ui, "TRANSFORM");
            }
            // A position or pixel lock rules out every transform here, as
            // the menus and canvas already say.
            let locked = self.lock_block(crate::layer_actions::LockNeed::Reshape);
            if let Some(why) = locked {
                ui.label(RichText::new(why).small().color(MUTED));
            }
            ui.add_enabled_ui(locked.is_none(), |ui| {
                slider_row(ui, "Scale", &mut self.xform_scale, 10.0..=400.0, "%");
                slider_row(ui, "Rotate", &mut self.xform_angle, -180.0..=180.0, "°");
            });
            let pending = (self.xform_scale - 100.0).abs() > 0.01 || self.xform_angle.abs() > 0.01;
            let mut apply = false;
            let mut flip = None;
            let mut free = false;
            ui.add_enabled_ui(locked.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    if ui
                        .add_enabled(pending, primary_button("Apply"))
                        .on_hover_text("Scale and rotate the layer about its centre")
                        .on_disabled_hover_text("Set a scale or angle first")
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui
                        .button("Flip H")
                        .on_hover_text("Flip the layer horizontally")
                        .clicked()
                    {
                        flip = Some(true);
                    }
                    if ui
                        .button("Flip V")
                        .on_hover_text("Flip the layer vertically")
                        .clicked()
                    {
                        flip = Some(false);
                    }
                    if ui
                        .add(
                            egui::Button::new("Free transform…")
                                .shortcut_text(self.action_keys(ui.ctx(), "xform")),
                        )
                        .on_hover_text("Transform on the canvas with handles")
                        .clicked()
                    {
                        free = true;
                    }
                })
            });
            // Scale and rotate about the painted centre (smart objects
            // compose this into their transform).
            let about = |sx: f32, sy: f32, a: f32| {
                pivot.map(|(cx, cy)| TransformLayer {
                    layer: id,
                    transform: lumenply_tiles::Affine::around(cx, cy, sx, sy, a),
                })
            };
            if apply {
                let s = self.xform_scale / 100.0;
                match about(s, s, self.xform_angle.to_radians()) {
                    Some(cmd) => {
                        self.run(&cmd);
                        self.xform_scale = 100.0;
                        self.xform_angle = 0.0;
                    }
                    None => self.status = "Layer has no pixels to transform".into(),
                }
            }
            if let Some(h) = flip {
                self.flip_active(h);
            }
            if rasterize {
                self.run(&RasterizeLayer { layer: id });
            }
            if let Some(action) = contents {
                self.run_menu_action(action);
                return;
            }
            if free {
                self.begin_free_transform();
            }
        }
        if can_fx {
            self.effects_ui(ui, id, effects);
        }
        self.histogram_footer(ui);
    }

    /// The layer-effects section, folded by default (the header says how
    /// many are on) so the layer's own settings stay in view.
    fn effects_ui(&mut self, ui: &mut egui::Ui, id: LayerId, fx: lumenply_doc::LayerEffects) {
        let on = [
            fx.drop_shadow.is_some(),
            fx.outer_glow.is_some(),
            fx.inner_shadow.is_some(),
            fx.inner_glow.is_some(),
            fx.bevel.is_some(),
            fx.color_overlay.is_some(),
            fx.gradient_overlay.is_some(),
            fx.stroke.is_some(),
        ]
        .into_iter()
        .filter(|b| *b)
        .count();
        let title = if on > 0 {
            format!("EFFECTS · {on} ON")
        } else {
            "EFFECTS".to_string()
        };
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new(title).small().strong().color(MUTED))
            .id_salt("layer-effects")
            .default_open(on > 0)
            .show(ui, |ui| self.effects_body(ui, id, fx));
    }

    /// Non-destructive layer effects: toggles and parameters, coalescing
    /// into one history step per drag.
    fn effects_body(&mut self, ui: &mut egui::Ui, id: LayerId, mut fx: lumenply_doc::LayerEffects) {
        use lumenply_doc::{GlowFx, ShadowFx, StrokeFx};
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
            if check(ui, &mut on, "Drop shadow").changed() {
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
                row_label(ui, "Offset", LABEL_W);
                let field = |ui: &mut egui::Ui, v: &mut f32, axis: &str| {
                    let dv = egui::DragValue::new(v)
                        .speed(0.5)
                        .range(-200.0..=200.0)
                        .fixed_decimals(0)
                        .prefix(axis)
                        .suffix(" px");
                    let r = num_field(ui, dv, 78.0);
                    a11y_name(&r, &format!("Offset {}", axis.trim()));
                    r
                };
                let rx = field(ui, &mut sfx.dx, "x ");
                let ry = field(ui, &mut sfx.dy, "y ");
                changed |= rx.changed() || ry.changed();
                finished |= rx.drag_stopped()
                    || ry.drag_stopped()
                    || (rx.changed() && !rx.dragged())
                    || (ry.changed() && !ry.dragged());
            });
            let f = slider_row(ui, "Blur", &mut sfx.blur, 0.0..=60.0, " px");
            finished |= f;
            let f2 = slider_row_scaled(ui, "Opacity", &mut sfx.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f2;
            if sfx != fx.drop_shadow.unwrap() {
                changed = true;
            }
            fx.drop_shadow = Some(sfx);
        }

        ui.horizontal(|ui| {
            let mut on = fx.outer_glow.is_some();
            if check(ui, &mut on, "Outer glow").changed() {
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
            let f2 = slider_row_scaled(ui, "Opacity", &mut g.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f2;
            if g != fx.outer_glow.unwrap() {
                changed = true;
            }
            fx.outer_glow = Some(g);
        }

        ui.horizontal(|ui| {
            let mut on = fx.inner_shadow.is_some();
            if check(ui, &mut on, "Inner shadow").changed() {
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
                row_label(ui, "Offset", LABEL_W);
                let field = |ui: &mut egui::Ui, v: &mut f32, axis: &str| {
                    let dv = egui::DragValue::new(v)
                        .speed(0.5)
                        .range(-200.0..=200.0)
                        .fixed_decimals(0)
                        .prefix(axis)
                        .suffix(" px");
                    let r = num_field(ui, dv, 78.0);
                    a11y_name(&r, &format!("Offset {}", axis.trim()));
                    r
                };
                let rx = field(ui, &mut sfx.dx, "x ");
                let ry = field(ui, &mut sfx.dy, "y ");
                changed |= rx.changed() || ry.changed();
                finished |= rx.drag_stopped()
                    || ry.drag_stopped()
                    || (rx.changed() && !rx.dragged())
                    || (ry.changed() && !ry.dragged());
            });
            let f = slider_row(ui, "Blur", &mut sfx.blur, 0.0..=60.0, " px");
            finished |= f;
            let f2 = slider_row_scaled(ui, "Opacity", &mut sfx.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f2;
            if sfx != fx.inner_shadow.unwrap() {
                changed = true;
            }
            fx.inner_shadow = Some(sfx);
        }

        ui.horizontal(|ui| {
            let mut on = fx.inner_glow.is_some();
            if check(ui, &mut on, "Inner glow").changed() {
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
            let f2 = slider_row_scaled(ui, "Opacity", &mut g.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f2;
            if g != fx.inner_glow.unwrap() {
                changed = true;
            }
            fx.inner_glow = Some(g);
        }

        ui.horizontal(|ui| {
            let mut on = fx.bevel.is_some();
            if check(ui, &mut on, "Bevel").changed() {
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
            let f4 = slider_row_scaled(ui, "Opacity", &mut b.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f4;
            if b != fx.bevel.unwrap() {
                changed = true;
            }
            fx.bevel = Some(b);
        }

        ui.horizontal(|ui| {
            let mut on = fx.color_overlay.is_some();
            if check(ui, &mut on, "Color overlay").changed() {
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
            let f = slider_row_scaled(ui, "Opacity", &mut co.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f;
            if co != fx.color_overlay.unwrap() {
                changed = true;
            }
            fx.color_overlay = Some(co);
        }

        ui.horizontal(|ui| {
            let mut on = fx.gradient_overlay.is_some();
            if check(ui, &mut on, "Gradient overlay").changed() {
                fx.gradient_overlay = on.then(lumenply_doc::GradientOverlayFx::default);
                changed = true;
                finished = true;
            }
            if let Some(go) = &mut fx.gradient_overlay {
                let c1 = color_btn(ui, &mut go.start);
                let c2 = color_btn(ui, &mut go.end);
                if c1 || c2 {
                    // Editing the end colours turns an imported multi-stop
                    // gradient back into the two-colour one shown here.
                    go.fill = None;
                }
                changed |= c1 || c2;
                finished |= c1 || c2;
            }
        });
        if let Some(mut go) = fx.gradient_overlay.clone() {
            let f = slider_row(ui, "Angle", &mut go.angle, 0.0..=360.0, "°");
            finished |= f;
            if let Some(lumenply_doc::Fill::Gradient { angle, .. }) = &mut go.fill {
                *angle = go.angle;
            }
            let f2 = slider_row_scaled(ui, "Opacity", &mut go.opacity, 0.0..=1.0, 100.0, "%");
            finished |= f2;
            if Some(&go) != fx.gradient_overlay.as_ref() {
                changed = true;
            }
            fx.gradient_overlay = Some(go);
        }

        ui.horizontal(|ui| {
            let mut on = fx.stroke.is_some();
            if check(ui, &mut on, "Stroke").changed() {
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
            ui.horizontal(|ui| {
                row_label(ui, "Position", LABEL_W);
                use lumenply_doc::StrokeAlign as A;
                let opts = [
                    (A::Outside, "Outside"),
                    (A::Center, "Center"),
                    (A::Inside, "Inside"),
                ];
                finished |= segmented(ui, &mut st.position, &opts);
            });
            let f2 = slider_row_scaled(ui, "Opacity", &mut st.opacity, 0.0..=1.0, 100.0, "%");
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
        let finished = self.adjustment_controls(ui, id, &mut adj);
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

    /// The controls for one adjustment's settings (also used by the
    /// Image ▸ Adjustments dialogs). True when an edit finished.
    pub(crate) fn adjustment_controls(
        &mut self,
        ui: &mut egui::Ui,
        id: LayerId,
        adj: &mut Adjustment,
    ) -> bool {
        let mut finished = false;
        match adj {
            Adjustment::Invert => {
                ui.label(RichText::new("This adjustment has no settings.").weak());
            }
            Adjustment::BrightnessContrast { brightness, contrast } => {
                finished |= slider_row_scaled(ui, "Brightness", brightness, -1.0..=1.0, 100.0, "");
                finished |= slider_row_scaled(ui, "Contrast", contrast, -1.0..=1.0, 100.0, "");
            }
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
                colorize,
            } => {
                if ui
                    .checkbox(colorize, "Colorize")
                    .on_hover_text("One hue and saturation for the whole image, keeping its lightness")
                    .changed()
                {
                    // Photoshop's starting point for each mode.
                    (*hue, *saturation) = if *colorize { (0.0, 0.25) } else { (0.0, 0.0) };
                    finished = true;
                }
                if *colorize {
                    finished |= slider_row(ui, "Hue", hue, 0.0..=360.0, "°");
                    finished |= slider_row_scaled(ui, "Saturation", saturation, 0.0..=1.0, 100.0, "");
                } else {
                    finished |= slider_row(ui, "Hue", hue, -180.0..=180.0, "°");
                    finished |= slider_row_scaled(ui, "Saturation", saturation, -1.0..=1.0, 100.0, "");
                }
                finished |= slider_row_scaled(ui, "Lightness", lightness, -1.0..=1.0, 100.0, "");
            }
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                channels,
            } => {
                segmented(
                    ui,
                    &mut self.levels_ch,
                    &[(0, "Master"), (1, "Red"), (2, "Green"), (3, "Blue")],
                );
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
                finished |= slider_row_scaled(ui, "Input black", ib, 0.0..=1.0, 255.0, "");
                finished |= slider_row_scaled(ui, "Input white", iw, 0.0..=1.0, 255.0, "");
                finished |= slider_row(ui, "Gamma", g, 0.1..=4.0, "");
                finished |= slider_row_scaled(ui, "Output black", ob, 0.0..=1.0, 255.0, "");
                finished |= slider_row_scaled(ui, "Output white", ow, 0.0..=1.0, 255.0, "");
            }
            Adjustment::Curves { points, channels } => {
                segmented(
                    ui,
                    &mut self.levels_ch,
                    &[(0, "Master"), (1, "Red"), (2, "Green"), (3, "Blue")],
                );
                let pts = match self.levels_ch {
                    0 => points,
                    n => &mut channels[n - 1],
                };
                // A straight channel is stored empty; edit it as the
                // diagonal without turning a mere look into an edit.
                let mut edit = if pts.len() < 2 {
                    vec![[0.0, 0.0], [1.0, 1.0]]
                } else {
                    pts.clone()
                };
                let shown = edit.clone();
                finished |= curve_editor(ui, &mut edit, &mut self.curve_drag);
                if edit != shown {
                    *pts = edit;
                }
            }
            Adjustment::BlackWhite { red, green, blue } => {
                finished |= slider_row_scaled(ui, "Red", red, 0.0..=1.0, 100.0, "%");
                finished |= slider_row_scaled(ui, "Green", green, 0.0..=1.0, 100.0, "%");
                finished |= slider_row_scaled(ui, "Blue", blue, 0.0..=1.0, 100.0, "%");
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
                segmented(
                    ui,
                    &mut self.cb_tone,
                    &[(0, "Shadows"), (1, "Midtones"), (2, "Highlights")],
                );
                let tone = match self.cb_tone {
                    0 => shadows,
                    1 => midtones,
                    _ => highlights,
                };
                // The pair labels are longer than the usual column.
                let o = RowOpts {
                    label_w: 112.0,
                    ..RowOpts::default()
                };
                finished |=
                    slider_row_scaled_w(ui, "Cyan ↔ Red", &mut tone[0], -1.0..=1.0, 100.0, "", o.label_w);
                finished |= slider_row_scaled_w(
                    ui,
                    "Magenta ↔ Green",
                    &mut tone[1],
                    -1.0..=1.0,
                    100.0,
                    "",
                    o.label_w,
                );
                finished |= slider_row_scaled_w(
                    ui,
                    "Yellow ↔ Blue",
                    &mut tone[2],
                    -1.0..=1.0,
                    100.0,
                    "",
                    o.label_w,
                );
                if check(ui, preserve_luminosity, "Preserve luminosity").changed() {
                    finished = true;
                }
            }
            Adjustment::Vibrance { vibrance, saturation } => {
                finished |= slider_row_scaled(ui, "Vibrance", vibrance, -1.0..=1.0, 100.0, "");
                finished |= slider_row_scaled(ui, "Saturation", saturation, -1.0..=1.0, 100.0, "");
            }
            Adjustment::Threshold { level } => {
                finished |= slider_row_scaled(ui, "Level", level, 0.0..=1.0, 255.0, "");
            }
            Adjustment::Posterize { levels } => {
                let mut v = *levels as f32;
                let o = RowOpts {
                    int: true,
                    ..RowOpts::default()
                };
                finished |= slider_row_ex(ui, "Levels", &mut v, 2.0..=32.0, "", o);
                *levels = (v.round() as u32).clamp(2, 32);
            }
            other => finished |= crate::adjust_ui::adjustment_ui(ui, id, other),
        }
        finished
    }
}

/// An interactive curve: drag points, click to add, right-click to remove.
pub(crate) fn curve_editor(ui: &mut egui::Ui, points: &mut Vec<[f32; 2]>, drag: &mut Option<usize>) -> bool {
    let size = Vec2::splat(210.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    resp.widget_info(|| {
        let label = format!("Curve, {} points", points.len());
        egui::WidgetInfo::labeled(egui::WidgetType::Other, true, label)
    });
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
        channels: Default::default(),
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
