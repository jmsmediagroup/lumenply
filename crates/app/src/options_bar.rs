use super::*;

impl App {
    // ---- options bar ----------------------------------------------------------------

    pub(crate) fn options_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("options")
            .exact_height(42.0)
            .frame(bar_frame())
            .show(ctx, |ui| {
                // Narrower windows get shorter sliders, a mode menu instead
                // of a row of mode buttons, and hints moved into tooltips;
                // whatever still doesn't fit scrolls sideways.
                // Start from the width; step down while this bar's content
                // was measured too wide at that tier (per tool and state,
                // so each tier's width is remembered and nothing flickers).
                let w = ui.available_width();
                let key = egui::Id::new((
                    "options-fit",
                    self.tool.name(),
                    self.editing_mask,
                    self.quick_mask,
                    self.xform.is_some(),
                    self.active_is_text(),
                    self.lasso.is_empty(),
                ));
                let tier = Tier::fit(w, |t| ui.data(|d| d.get_temp::<f32>(key.with(t))));
                let measured_before = ui.data(|d| d.get_temp::<f32>(key.with(tier))).is_some();
                let used = bar_scroll(ui, |ui| {
                    ui.spacing_mut().slider_width = tier.slider_w();
                    if let Some(mut x) = self.xform.clone() {
                        ui.label(
                            RichText::new("Free Transform")
                                .family(egui::FontFamily::Name("semibold".into()))
                                .color(TEXT),
                        );
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
                            let field = |ui: &mut egui::Ui, label: &str, dv: egui::DragValue| {
                                ui.label(RichText::new(label).color(MUTED));
                                num_field(ui, dv.fixed_decimals(1), 70.0).changed()
                            };
                            let c1 = field(ui, "W", egui::DragValue::new(&mut sx).speed(1.0).suffix("%"));
                            let c2 = field(ui, "H", egui::DragValue::new(&mut sy).speed(1.0).suffix("%"));
                            let c3 = field(ui, "Rotate", egui::DragValue::new(&mut rot).speed(0.5).suffix("°"));
                            let c4 = field(
                                ui,
                                "Skew",
                                egui::DragValue::new(&mut skew)
                                    .speed(0.5)
                                    .range(-80.0..=80.0)
                                    .suffix("°"),
                            );
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
                        if ui
                            .add(
                                primary_button("Apply")
                                    .shortcut_text(RichText::new("Enter").color(ACCENT_INK.gamma_multiply(0.7))),
                            )
                            .on_hover_text("Commit the transform (Enter)")
                            .clicked()
                        {
                            self.commit_free_transform();
                        }
                        if ui
                            .add(footer_button("Cancel").shortcut_text("Esc"))
                            .on_hover_text("Drop the transform (Esc)")
                            .clicked()
                        {
                            self.cancel_free_transform();
                        }
                        return;
                    }
                    // The tool's name; its full tip (and, on narrow windows,
                    // the usage hint the bar has no room for) on hover.
                    let hint = tool_hint(self.tool);
                    let tip = match (tier, hint) {
                        (Tier::Tight, Some(h)) => format!("{}\n{h}", self.tool.tip()),
                        _ => self.tool.tip().to_string(),
                    };
                    ui.label(
                        RichText::new(self.tool.name())
                            .family(egui::FontFamily::Name("semibold".into()))
                            .color(TEXT),
                    )
                    .on_hover_text(tip);
                    if self.quick_mask {
                        let chip = RichText::new("QUICK MASK").small().color(DANGER);
                        ui.add(
                            egui::Button::new(chip)
                                .fill(PANEL)
                                .stroke(Stroke::new(1.0, DANGER))
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
                            if ui
                                .add_enabled(
                                    self.active_is_pixel(),
                                    egui::Button::new("Free transform").shortcut_text("Ctrl+T"),
                                )
                                .on_hover_text("Scale, rotate, skew, distort or warp the active layer")
                                .on_disabled_hover_text("Select a pixel layer to transform it")
                                .clicked()
                            {
                                self.begin_free_transform();
                            }
                            hint_label(ui, tier, self.tool);
                        }
                        Tool::Eyedropper => {
                            hint_label(ui, tier, self.tool);
                        }
                        Tool::Bucket | Tool::Wand => {
                            let mut tol = self.tolerance * 100.0;
                            if bar_slider(ui, "Tolerance", &mut tol, 0.0..=100.0, "%", false) {
                                self.tolerance = tol / 100.0;
                            }
                            ui.checkbox(&mut self.contiguous, "Contiguous")
                                .on_hover_text("Only fill/select connected pixels");
                            ui.checkbox(&mut self.sample_merged, "All layers")
                                .on_hover_text("Sample the merged image instead of the active layer only");
                            if self.tool == Tool::Bucket {
                                let mut op = self.brush.color[3] * 100.0;
                                if bar_slider(ui, "Opacity", &mut op, 1.0..=100.0, "%", false) {
                                    self.brush.color[3] = op / 100.0;
                                }
                            } else {
                                ui.separator();
                                select_ops(ui, &mut self.select_op);
                            }
                        }
                        Tool::Text => self.text_options_bar(ui),
                        Tool::Gradient => {
                            segmented(
                                ui,
                                &mut self.gradient_kind,
                                &[(GradientKind::Linear, "Linear"), (GradientKind::Radial, "Radial")],
                            );
                            ui.separator();
                            ui.label("From");
                            crate::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                            ui.label("To");
                            crate::color_picker::color_edit_button_rgb(ui, &mut self.bg_rgb);
                            ui.checkbox(&mut self.gradient_to_transparent, "To transparent");
                            hint_label(ui, tier, self.tool);
                        }
                        Tool::Brush | Tool::Eraser | Tool::Clone | Tool::Heal => {
                            if self.tool == Tool::Brush {
                                const MODES: [(BrushMode, &str); 6] = [
                                    (BrushMode::Paint, "Paint"),
                                    (BrushMode::Dodge, "Dodge"),
                                    (BrushMode::Burn, "Burn"),
                                    (BrushMode::Smudge, "Smudge"),
                                    (BrushMode::Saturate, "Sat+"),
                                    (BrushMode::Desaturate, "Sat−"),
                                ];
                                if tier == Tier::Wide {
                                    segmented(ui, &mut self.brush.mode, &MODES);
                                } else {
                                    // Narrow: the six modes fold into a menu.
                                    let current = MODES
                                        .iter()
                                        .find(|(m, _)| *m == self.brush.mode)
                                        .map_or("Paint", |(_, l)| *l);
                                    egui::ComboBox::from_id_salt("brush-mode")
                                        .selected_text(current)
                                        .width(86.0)
                                        .show_ui(ui, |ui| {
                                            for (m, label) in MODES {
                                                ui.selectable_value(&mut self.brush.mode, m, label);
                                            }
                                        })
                                        .response
                                        .on_hover_text("Brush mode");
                                }
                                ui.separator();
                                self.brush_presets_ui(ui, tier);
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
                                    Some((x, y)) => ui.label(
                                        RichText::new(format!("Source {:.0}, {:.0}", x, y)).monospace(),
                                    ),
                                    None => ui.label(RichText::new("No source yet").color(MUTED)),
                                };
                                ui.checkbox(&mut self.sample_merged, "All layers")
                                    .on_hover_text("Sample the merged image instead of the active layer only");
                            }
                            if self.tool == Tool::Heal || needs_source {
                                ui.separator();
                            }
                            let mut size = self.brush.radius * 2.0;
                            if bar_slider(ui, "Size", &mut size, 2.0..=400.0, " px", true) {
                                self.brush.radius = size / 2.0;
                            }
                            let mut hard = self.brush.hardness * 100.0;
                            if bar_slider(ui, "Hardness", &mut hard, 0.0..=100.0, "%", false) {
                                self.brush.hardness = hard / 100.0;
                            }
                            let mut op = self.brush.color[3] * 100.0;
                            if bar_slider(ui, "Opacity", &mut op, 1.0..=100.0, "%", false) {
                                self.brush.color[3] = op / 100.0;
                            }
                            let mut sc = self.brush.jitter * 100.0;
                            if bar_slider(ui, "Scatter", &mut sc, 0.0..=100.0, "%", false) {
                                self.brush.jitter = sc / 100.0;
                            }
                            if self.tool == Tool::Brush && self.editing_mask {
                                ui.separator();
                                if ui
                                    .button("White")
                                    .on_hover_text("Paint white: reveal the layer")
                                    .clicked()
                                {
                                    self.brush_rgb = [1.0; 3];
                                }
                                if ui
                                    .button("Black")
                                    .on_hover_text("Paint black: hide the layer")
                                    .clicked()
                                {
                                    self.brush_rgb = [0.0; 3];
                                }
                            }
                        }
                        Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::PolyLasso => {
                            if self.tool == Tool::PolyLasso && !self.lasso.is_empty() {
                                if ui
                                    .add(primary_button("Close"))
                                    .on_hover_text("Close the polygon into a selection")
                                    .clicked()
                                {
                                    self.finish_polygon(ctx);
                                }
                                if ui.button("Cancel").clicked() {
                                    self.lasso.clear();
                                }
                                ui.separator();
                            }
                            select_ops(ui, &mut self.select_op);
                            ui.separator();
                            bar_slider(ui, "Feather", &mut self.feather, 0.0..=100.0, " px", false);
                            let has_sel = self.editor.doc().selection.is_some();
                            if ui
                                .add_enabled(has_sel, egui::Button::new("Apply"))
                                .on_hover_text("Feather the current selection by this radius")
                                .on_disabled_hover_text("Make a selection first")
                                .clicked()
                            {
                                self.run(&FeatherSelection { radius: self.feather });
                            }
                            hint_label(ui, tier, self.tool);
                        }
                        Tool::Pen => {
                            let has_path = self.editor.doc().work_path.is_some();
                            let on_pixels = self.active_is_pixel();
                            let need_path = "Draw a path on the canvas first";
                            let need_pixels = if has_path {
                                "Select a pixel layer to paint the path onto"
                            } else {
                                need_path
                            };
                            if ui
                                .add_enabled(has_path && on_pixels, egui::Button::new("Fill path"))
                                .on_hover_text("Fill the path's area with the brush colour")
                                .on_disabled_hover_text(need_pixels)
                                .clicked()
                            {
                                if let Some(layer) = self.active {
                                    let color = self.make_brush().color;
                                    self.run(&FillPath { layer, color });
                                }
                            }
                            if ui
                                .add_enabled(has_path && on_pixels, egui::Button::new("Stroke path"))
                                .on_hover_text("Paint along the path with the current brush")
                                .on_disabled_hover_text(need_pixels)
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
                                .on_hover_text("Turn the closed path into a selection")
                                .on_disabled_hover_text(need_path)
                                .clicked()
                            {
                                self.run(&PathToSelection {
                                    op: self.select_op,
                                });
                            }
                            if ui
                                .add_enabled(has_path, egui::Button::new("Clear path"))
                                .on_hover_text("Delete the work path")
                                .on_disabled_hover_text(need_path)
                                .clicked()
                            {
                                self.pen_open = false;
                                self.run(&SetWorkPath { path: None });
                            }
                            ui.separator();
                            if ui
                                .add_enabled(has_path, egui::Button::new("Save path"))
                                .on_hover_text("Keep a named copy in the document's Paths list")
                                .on_disabled_hover_text(need_path)
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
                                        popup_style(ui);
                                        for (i, name) in names.iter().enumerate() {
                                            ui.horizontal(|ui| {
                                                if ui.selectable_label(false, name).clicked() {
                                                    load = Some(i);
                                                }
                                                if ui
                                                    .small_button("×")
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
                            hint_label(ui, tier, self.tool);
                        }
                        Tool::Hand => {
                            hint_label(ui, tier, self.tool);
                        }
                    }
                });
                ui.data_mut(|d| d.insert_temp(key.with(tier), used));
                if !measured_before && used > w && tier.narrower().is_some() {
                    // First sight of this bar at this tier and it overflows:
                    // lay out again right away at the narrower tier.
                    ui.ctx().request_repaint();
                }
            });
    }

    /// Preset dropdown + save button for the brush's shape parameters.
    fn brush_presets_ui(&mut self, ui: &mut egui::Ui, tier: Tier) {
        let save = if tier == Tier::Tight {
            "Save"
        } else {
            "Save preset"
        };
        if ui
            .button(save)
            .on_hover_text("Save a brush preset: the current size, hardness, opacity, spacing and scatter")
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
                popup_style(ui);
                for (i, p) in self.prefs.brush_presets.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(false, &p.name).clicked() {
                            apply = Some(i);
                        }
                        if ui.small_button("×").on_hover_text("Delete this preset").clicked() {
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

/// How much horizontal room the options bar has.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Tier {
    /// Everything at full size.
    Wide,
    /// Shorter sliders; brush modes fold into a menu.
    Compact,
    /// Shortest sliders; usage hints move into the tool name's tooltip.
    Tight,
}

impl Tier {
    fn for_width(w: f32) -> Tier {
        if w >= 1400.0 {
            Tier::Wide
        } else if w >= 1100.0 {
            Tier::Compact
        } else {
            Tier::Tight
        }
    }

    /// The tier for a bar `w` wide: by width, then stepped down while the
    /// content was measured (`needed`) wider than the bar at that tier.
    fn fit(w: f32, needed: impl Fn(Tier) -> Option<f32>) -> Tier {
        let mut tier = Tier::for_width(w);
        while let Some(narrower) = tier.narrower() {
            match needed(tier) {
                Some(need) if need > w => tier = narrower,
                _ => break,
            }
        }
        tier
    }

    /// The next more compact tier, if any.
    fn narrower(self) -> Option<Tier> {
        match self {
            Tier::Wide => Some(Tier::Compact),
            Tier::Compact => Some(Tier::Tight),
            Tier::Tight => None,
        }
    }

    fn slider_w(self) -> f32 {
        match self {
            Tier::Wide => 110.0,
            Tier::Compact => 84.0,
            Tier::Tight => 56.0,
        }
    }
}

/// The bar's content in a sideways scroll area (wheel scrolls it too), so
/// no control is ever cut off; a fade marks an edge with more beyond it.
/// Returns the content's width.
fn bar_scroll(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> f32 {
    raise_controls(ui);
    ui.style_mut().always_scroll_the_only_direction = true;
    let out = egui::ScrollArea::horizontal()
        .id_salt("options-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_centered(add);
        });
    let r = out.inner_rect;
    let hidden_right = out.content_size.x - out.state.offset.x - r.width();
    let fade = |a: egui::Pos2, b: egui::Pos2, from: Color32, to: Color32| {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(egui::pos2(a.x, r.min.y), from);
        mesh.colored_vertex(egui::pos2(b.x, r.min.y), to);
        mesh.colored_vertex(egui::pos2(b.x, r.max.y), to);
        mesh.colored_vertex(egui::pos2(a.x, r.max.y), from);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        ui.painter().add(Shape::mesh(mesh));
    };
    if hidden_right > 1.0 {
        fade(
            egui::pos2(r.max.x - 32.0, 0.0),
            egui::pos2(r.max.x, 0.0),
            Color32::TRANSPARENT,
            PANEL,
        );
    }
    if out.state.offset.x > 1.0 {
        fade(
            egui::pos2(r.min.x, 0.0),
            egui::pos2(r.min.x + 32.0, 0.0),
            PANEL,
            Color32::TRANSPARENT,
        );
    }
    out.content_size.x
}

/// A slider cluster in the bar: muted label, slider, typeable mono value.
/// Returns true when the value changed.
fn bar_slider(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    log: bool,
) -> bool {
    // Tighter spacing inside the cluster than between clusters.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(label).color(MUTED));
        let span = range.end() - range.start();
        let a = ui
            .add(
                egui::Slider::new(v, range.clone())
                    .logarithmic(log)
                    .show_value(false),
            )
            .on_hover_text(label)
            .changed();
        let b = num_field(
            ui,
            egui::DragValue::new(v)
                .range(range)
                .speed(span / 300.0)
                .fixed_decimals(0)
                .suffix(suffix),
            58.0,
        )
        .changed();
        a || b
    })
    .inner
}

/// New / Add / Subtract / Intersect for the selection tools.
fn select_ops(ui: &mut egui::Ui, op: &mut CombineOp) {
    segmented(
        ui,
        op,
        &[
            (CombineOp::Replace, "New"),
            (CombineOp::Union, "Add"),
            (CombineOp::Subtract, "Subtract"),
            (CombineOp::Intersect, "Intersect"),
        ],
    );
}

/// How to use a tool, in a phrase.
fn tool_hint(tool: Tool) -> Option<&'static str> {
    Some(match tool {
        Tool::Move => "Drag to move the active layer",
        Tool::Eyedropper => "Click to pick the brush colour from the image",
        Tool::Gradient => "Drag on the canvas",
        Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso => "Shift adds, Alt subtracts",
        Tool::PolyLasso => "Click to add points, double-click to close",
        Tool::Pen => "Click corners, drag curves; click the first point to close",
        Tool::Hand => "Drag to pan, scroll to zoom",
        _ => return None,
    })
}

/// The tool's usage hint at the end of the bar (on narrow windows it lives
/// in the tool name's tooltip instead).
fn hint_label(ui: &mut egui::Ui, tier: Tier, tool: Tool) {
    if let (false, Some(h)) = (tier == Tier::Tight, tool_hint(tool)) {
        ui.label(RichText::new(h).weak());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_tiers_follow_the_window_width() {
        assert!(Tier::for_width(1600.0) == Tier::Wide);
        assert!(Tier::for_width(1400.0) == Tier::Wide);
        assert!(Tier::for_width(1399.0) == Tier::Compact);
        assert!(Tier::for_width(1100.0) == Tier::Compact);
        assert!(Tier::for_width(1024.0) == Tier::Tight);
        assert_eq!(Tier::Wide.slider_w(), 110.0);
        assert_eq!(Tier::Compact.slider_w(), 84.0);
        assert_eq!(Tier::Tight.slider_w(), 56.0);
    }

    #[test]
    fn an_overflowing_bar_steps_down_a_tier() {
        // Brush on a mask at 1600 px: 1700 px wide at Wide, 1290 at Compact.
        let need = |t: Tier| match t {
            Tier::Wide => Some(1700.0),
            Tier::Compact => Some(1290.0),
            Tier::Tight => Some(1100.0),
        };
        assert!(Tier::fit(1600.0, need) == Tier::Compact);
        assert!(Tier::fit(1800.0, need) == Tier::Wide);
        assert!(Tier::fit(1250.0, need) == Tier::Tight);
        // Nothing measured yet: the width alone decides.
        assert!(Tier::fit(1600.0, |_| None) == Tier::Wide);
        // Tight is the floor even when it still overflows.
        assert!(Tier::fit(900.0, need) == Tier::Tight);
    }

    #[test]
    fn hints_cover_the_tools_that_need_one() {
        assert_eq!(tool_hint(Tool::Hand), Some("Drag to pan, scroll to zoom"));
        assert_eq!(tool_hint(Tool::Lasso), Some("Shift adds, Alt subtracts"));
        assert_eq!(tool_hint(Tool::Brush), None);
        assert_eq!(tool_hint(Tool::Text), None);
    }
}
