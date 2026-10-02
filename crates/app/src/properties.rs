use super::*;

impl App {
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

        let r = ui.add(
            egui::Slider::new(&mut opacity, 0.0..=100.0)
                .suffix("%")
                .text("Opacity"),
        );
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

        let mut b = blend;
        egui::ComboBox::from_label("Blend mode")
            .selected_text(b.name())
            .show_ui(ui, |ui| {
                for m in BlendMode::ALL {
                    ui.selectable_value(&mut b, m, m.name());
                }
            });
        if b != blend {
            self.run(&SetBlendMode { layer: id, blend: b });
        }

        if let Some(adj) = adj {
            ui.add_space(4.0);
            ui.label(RichText::new(adj.name()).small().strong());
            self.adjustment_ui(ui, id, adj);
        } else if let Some(t) = self.active_text() {
            ui.add_space(4.0);
            ui.label(RichText::new("Text").small().strong());
            self.text_controls(ui, id, t, true);
            if ui.button("Rasterize").clicked() {
                self.run(&RasterizeLayer { layer: id });
            }
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
            } => {
                finished |= slider_row(ui, "Input black", in_black, 0.0..=1.0, "");
                finished |= slider_row(ui, "Input white", in_white, 0.0..=1.0, "");
                finished |= slider_row(ui, "Gamma", gamma, 0.1..=4.0, "");
                finished |= slider_row(ui, "Output black", out_black, 0.0..=1.0, "");
                finished |= slider_row(ui, "Output white", out_white, 0.0..=1.0, "");
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
