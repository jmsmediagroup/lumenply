//! The Shape tool (U): Photoshop's grouped shape tools in one rail entry.
//! The options bar picks the kind (rectangle, rounded rectangle, ellipse,
//! polygon, line, or a custom shape), the fill and the stroke; a drag on
//! the canvas draws a live preview and commits one `AddShapeLayer`.
//! Shift constrains (square, circle, 45° lines), Alt draws from the
//! centre, and the press and drag points snap (View ▸ Snap). The
//! Properties panel edits the active shape layer through `SetShape`.

use super::*;
use crate::options_bar::{bar_slider, Tier};
use lumenply_doc::shape::{
    drag_box, CustomShape, ShapeGeometry, ShapeKind, ShapeLayer, ShapeParams, ShapeStroke, StrokeAlign,
};
use lumenply_doc::{Fill, Gradient, GradientStyle};
use lumenply_io::{linear_to_srgb_f, srgb_to_linear_f};

/// What the next shape's inside is painted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FillMode {
    None,
    Solid,
    Gradient,
}

/// The Shape tool's options and the drag in progress.
pub(crate) struct ShapeTool {
    pub(crate) params: ShapeParams,
    pub(crate) fill: FillMode,
    /// sRGB swatches, as the colour well holds them.
    pub(crate) fill_rgb: [f32; 3],
    pub(crate) stroke_on: bool,
    pub(crate) stroke_rgb: [f32; 3],
    pub(crate) stroke_width: f32,
    pub(crate) align: StrokeAlign,
    /// The press point of the drag in progress (document space, snapped).
    pub(crate) start: Option<(f32, f32)>,
    /// The constrained drag ends of the current preview.
    pub(crate) current: Option<DragEnds>,
    /// Canvas area the last preview painted, to clean it up.
    pub(crate) last_area: Option<Rect>,
}

impl Default for ShapeTool {
    fn default() -> Self {
        ShapeTool {
            params: ShapeParams::default(),
            fill: FillMode::Solid,
            fill_rgb: [0.24, 0.52, 0.92],
            stroke_on: false,
            stroke_rgb: [0.0, 0.0, 0.0],
            stroke_width: 3.0,
            align: StrokeAlign::Inside,
            start: None,
            current: None,
            last_area: None,
        }
    }
}

/// Every pickable kind for the one combo box: the five basic kinds, then
/// each custom shape.
fn kind_entries() -> Vec<(ShapeKind, CustomShape, &'static str)> {
    let mut v: Vec<_> = ShapeKind::ALL
        .into_iter()
        .filter(|k| *k != ShapeKind::Custom)
        .map(|k| (k, CustomShape::Star, k.name()))
        .collect();
    v.extend(
        CustomShape::ALL
            .into_iter()
            .map(|c| (ShapeKind::Custom, c, c.name())),
    );
    v
}

fn kind_label(p: &ShapeParams) -> &'static str {
    match p.kind {
        ShapeKind::Custom => p.custom.name(),
        k => k.name(),
    }
}

/// A labelled value in the options bar: a slider plus field, or on a
/// tight bar just the field.
fn bar_value(
    ui: &mut egui::Ui,
    tier: Tier,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    log: bool,
) -> bool {
    if tier != Tier::Tight {
        return bar_slider(ui, label, v, range, suffix, log);
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(label).color(MUTED));
        let span = range.end() - range.start();
        let r = num_field(
            ui,
            egui::DragValue::new(v)
                .range(range)
                .speed(span / 300.0)
                .fixed_decimals(0)
                .suffix(suffix),
            58.0,
        );
        a11y_name(&r, label);
        r.changed()
    })
    .inner
}

/// A drag's two constrained ends (document space).
type DragEnds = ((f32, f32), (f32, f32));

fn lin(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(srgb_to_linear_f)
}

impl ShapeTool {
    /// The fill and stroke the next shape gets; gradients run from the
    /// fill colour to `bg` (sRGB swatches in, linear out).
    pub(crate) fn paint(&self, bg: [f32; 3]) -> (Option<Fill>, Option<ShapeStroke>) {
        let fill = match self.fill {
            FillMode::None => None,
            FillMode::Solid => Some(Fill::Solid {
                color: lin(self.fill_rgb),
            }),
            FillMode::Gradient => Some(Fill::gradient(Gradient::two(lin(self.fill_rgb), lin(bg)))),
        };
        let stroke = self.stroke_on.then(|| ShapeStroke {
            color: lin(self.stroke_rgb),
            width: self.stroke_width,
            align: self.align,
            dash: None,
        });
        (fill, stroke)
    }

    /// The shape a drag from `a` to `b` draws, or `None` while it is too
    /// small to see (under a pixel across).
    pub(crate) fn shape_for(
        &self,
        a: (f32, f32),
        b: (f32, f32),
        shift: bool,
        alt: bool,
        bg: [f32; 3],
    ) -> Option<(ShapeLayer, DragEnds)> {
        let (p, q) = drag_box(self.params.kind, a, b, shift, alt);
        let big = if self.params.kind == ShapeKind::Line {
            (q.0 - p.0).hypot(q.1 - p.1) >= 1.0
        } else {
            (q.0 - p.0) >= 1.0 && (q.1 - p.1) >= 1.0
        };
        if !big {
            return None;
        }
        let (fill, stroke) = self.paint(bg);
        // A shape with neither fill nor stroke would be invisible: give it
        // a hairline so it can be seen and selected.
        let stroke = if fill.is_none() && stroke.is_none() {
            Some(ShapeStroke {
                width: 1.0,
                ..ShapeStroke::default()
            })
        } else {
            stroke
        };
        Some((
            ShapeLayer::new(ShapeGeometry::from_drag(&self.params, p, q), fill, stroke),
            (p, q),
        ))
    }
}

impl App {
    /// Canvas input for the Shape tool: press, drag (live preview), release
    /// (one undo step).
    pub(crate) fn shape_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.drag_started_by(primary) {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                self.begin_snap(&[]);
                let a = self.snap_point(to_doc(p));
                self.shape.start = Some(a);
                self.shape.current = None;
                self.shape.last_area = None;
                self.drag = Some(DragKind::Shape);
            }
        }
        if self.drag == Some(DragKind::Shape) && resp.dragged_by(primary) {
            if let (Some(a), Some(p)) = (self.shape.start, resp.interact_pointer_pos()) {
                let b = self.snap_point(to_doc(p));
                self.preview_shape(ctx, a, b);
            }
        }
        if resp.drag_stopped() && self.drag == Some(DragKind::Shape) {
            self.drag = None;
            let end = resp
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
            let mods = ctx.input(|i| i.modifiers);
            let start = self.shape.start.take();
            self.shape.current = None;
            let made = match (start, end) {
                (Some(a), Some(p)) => {
                    let b = self.snap_point(to_doc(p));
                    self.shape.shape_for(a, b, mods.shift, mods.alt, self.bg_rgb)
                }
                _ => None,
            };
            self.end_snap();
            match made {
                Some((shape, _)) => self.add_shape(shape),
                None => {
                    // Too small: drop the preview.
                    let area = self.shape.last_area.take();
                    self.mark(area);
                }
            }
            self.shape.last_area = None;
        } else if resp.clicked_by(primary) {
            self.status = "Drag to draw a shape: Shift constrains, Alt draws from the centre".into();
        }
    }

    /// Commit a drawn shape as a new layer above the active one.
    pub(crate) fn add_shape(&mut self, shape: ShapeLayer) {
        let new_id = self.editor.doc().next_id();
        self.run(&AddShapeLayer {
            shape,
            name: None,
            above: self.active,
        });
        if self.editor.doc().layer(new_id).is_some() {
            self.set_active(Some(new_id));
        }
    }

    /// Paint the shape a drag from `a` to `b` would make over the canvas.
    fn preview_shape(&mut self, ctx: &egui::Context, a: (f32, f32), b: (f32, f32)) {
        let mods = ctx.input(|i| i.modifiers);
        let made = self.shape.shape_for(a, b, mods.shift, mods.alt, self.bg_rgb);
        let canvas = self.editor.doc().canvas();
        let mut preview = self.editor.doc().clone();
        let area = match made {
            Some((shape, ends)) => {
                self.shape.current = Some(ends);
                let area = lumenply_core::shape_cmds::shape_area(&shape, canvas);
                let cmd = AddShapeLayer {
                    shape,
                    name: None,
                    above: self.active,
                };
                if cmd.apply(&mut preview).is_err() {
                    return;
                }
                Some(area)
            }
            None => {
                self.shape.current = None;
                None
            }
        };
        let paint = match (area, self.shape.last_area) {
            (Some(a), Some(b)) => Some(a.union(&b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => return,
        };
        self.preview(ctx, &preview, paint);
        self.shape.last_area = area;
        if let Some(((x0, y0), (x1, y1))) = self.shape.current {
            self.status = if self.shape.params.kind == ShapeKind::Line {
                format!(
                    "Line {:.0} px at {:.0}°",
                    (x1 - x0).hypot(y1 - y0),
                    -(y1 - y0).atan2(x1 - x0).to_degrees()
                )
            } else {
                format!(
                    "{} {:.0} × {:.0} px",
                    kind_label(&self.shape.params),
                    x1 - x0,
                    y1 - y0
                )
            };
        }
    }

    /// The outline of the shape being dragged, over its preview.
    pub(crate) fn paint_shape_overlay(&self, painter: &egui::Painter, resp: &egui::Response) {
        if self.drag != Some(DragKind::Shape) {
            return;
        }
        let Some((p, q)) = self.shape.current else { return };
        let origin = resp.rect.min + self.pan;
        let zoom = self.zoom;
        let ts = |x: f32, y: f32| egui::pos2(origin.x + x * zoom, origin.y + y * zoom);
        let path = ShapeGeometry::from_drag(&self.shape.params, p, q).path();
        for (pts, closed) in path.flatten() {
            let mut line: Vec<Pos2> = pts.iter().map(|&(x, y)| ts(x, y)).collect();
            if closed {
                if let Some(f) = line.first().copied() {
                    line.push(f);
                }
            }
            painter.add(Shape::line(
                line.clone(),
                Stroke::new(2.0, Color32::from_black_alpha(120)),
            ));
            painter.add(Shape::line(line, Stroke::new(1.0, ACCENT)));
        }
    }

    /// The Shape tool's options bar: kind, fill, stroke, and the kind's
    /// own setting (corner radius, sides, line weight and arrowheads).
    pub(crate) fn shape_options_bar(&mut self, ui: &mut egui::Ui, tier: Tier) {
        let st = &mut self.shape;
        let r = egui::ComboBox::from_id_salt("shape-kind")
            .selected_text(kind_label(&st.params))
            .width(if tier == Tier::Wide { 150.0 } else { 120.0 })
            .show_ui(ui, |ui| {
                popup_style(ui);
                for (i, (k, c, label)) in kind_entries().into_iter().enumerate() {
                    if i == ShapeKind::ALL.len() - 1 {
                        ui.separator();
                    }
                    let on = st.params.kind == k && (k != ShapeKind::Custom || st.params.custom == c);
                    if ui.selectable_label(on, label).clicked() {
                        st.params.kind = k;
                        st.params.custom = c;
                    }
                }
            })
            .response;
        a11y_name(&r, "Shape kind");
        r.on_hover_text("Which shape a drag draws");
        match st.params.kind {
            ShapeKind::RoundedRectangle => {
                bar_value(
                    ui,
                    tier,
                    "Radius",
                    &mut st.params.radius,
                    0.0..=500.0,
                    " px",
                    true,
                );
            }
            ShapeKind::Polygon => {
                let mut sides = st.params.sides as f32;
                if bar_value(ui, tier, "Sides", &mut sides, 3.0..=24.0, "", false) {
                    st.params.sides = sides.round().clamp(3.0, 24.0) as u32;
                }
            }
            ShapeKind::Line => {
                bar_value(
                    ui,
                    tier,
                    "Weight",
                    &mut st.params.weight,
                    1.0..=200.0,
                    " px",
                    true,
                );
                check(ui, &mut st.params.arrow_start, "Start").on_hover_text("Arrowhead at the start");
                check(ui, &mut st.params.arrow_end, "End").on_hover_text("Arrowhead at the end");
            }
            _ => {}
        }
        ui.separator();
        ui.label(RichText::new("Fill").color(MUTED));
        let r = egui::ComboBox::from_id_salt("shape-fill")
            .selected_text(match st.fill {
                FillMode::None => "None",
                FillMode::Solid => "Solid",
                FillMode::Gradient => "Gradient",
            })
            .width(84.0)
            .show_ui(ui, |ui| {
                popup_style(ui);
                ui.selectable_value(&mut st.fill, FillMode::None, "None");
                ui.selectable_value(&mut st.fill, FillMode::Solid, "Solid");
                ui.selectable_value(&mut st.fill, FillMode::Gradient, "Gradient");
            })
            .response;
        a11y_name(&r, "Shape fill");
        r.on_hover_text("Gradients run from the fill colour to the background colour");
        if st.fill != FillMode::None {
            crate::color_picker::color_edit_button_rgb(ui, &mut st.fill_rgb);
        }
        ui.separator();
        check(ui, &mut st.stroke_on, "Stroke").on_hover_text("Outline the shape");
        if st.stroke_on {
            crate::color_picker::color_edit_button_rgb(ui, &mut st.stroke_rgb);
            bar_value(ui, tier, "Width", &mut st.stroke_width, 0.5..=100.0, " px", true);
        }
        // A tight bar leaves the alignment to Properties.
        if st.stroke_on && tier != Tier::Tight {
            let r = egui::ComboBox::from_id_salt("shape-align")
                .selected_text(st.align.name())
                .width(72.0)
                .show_ui(ui, |ui| {
                    popup_style(ui);
                    for a in StrokeAlign::ALL {
                        ui.selectable_value(&mut st.align, a, a.name());
                    }
                })
                .response;
            a11y_name(&r, "Stroke alignment");
            r.on_hover_text("Where the stroke sits on the outline");
        }
        crate::options_bar::hint_label(ui, tier, Tool::Shape);
    }

    /// Properties of a shape layer: fill, stroke and the geometry's own
    /// settings, each edit one coalesced `SetShape`.
    pub(crate) fn shape_properties(&mut self, ui: &mut egui::Ui, id: LayerId, mut shape: ShapeLayer) {
        let before = shape.clone();
        let mut finished = false;
        let color_row = |ui: &mut egui::Ui, label: &str, color: &mut [f32; 3], finished: &mut bool| {
            ui.horizontal(|ui| {
                row_label(ui, label, LABEL_W);
                let mut srgb = color.map(linear_to_srgb_f);
                let r = crate::color_picker::color_edit_button_rgb(ui, &mut srgb);
                if r.changed() {
                    *color = srgb.map(srgb_to_linear_f);
                }
                *finished |= r.drag_stopped() || (r.changed() && !r.dragged());
            });
        };

        // Fill.
        ui.horizontal(|ui| {
            row_label(ui, "Fill", LABEL_W);
            let mut mode = match &shape.fill {
                None => FillMode::None,
                Some(Fill::Solid { .. }) => FillMode::Solid,
                Some(Fill::Gradient { .. }) => FillMode::Gradient,
            };
            let was = mode;
            segmented(
                ui,
                &mut mode,
                &[
                    (FillMode::None, "None"),
                    (FillMode::Solid, "Solid"),
                    (FillMode::Gradient, "Gradient"),
                ],
            );
            if mode != was {
                let first = match &shape.fill {
                    Some(Fill::Solid { color }) => *color,
                    Some(Fill::Gradient { gradient, .. }) => gradient.sorted()[0].color,
                    None => lin(self.shape.fill_rgb),
                };
                shape.fill = match mode {
                    FillMode::None => None,
                    FillMode::Solid => Some(Fill::Solid { color: first }),
                    FillMode::Gradient => Some(Fill::gradient(Gradient::two(first, [1.0; 3]))),
                };
                finished = true;
            }
        });
        match &mut shape.fill {
            Some(Fill::Solid { color }) => color_row(ui, "Color", color, &mut finished),
            Some(Fill::Gradient {
                gradient,
                style,
                angle,
                reverse,
                ..
            }) => {
                finished |= crate::adjust_ui::gradient_editor(ui, id, gradient, true).finished;
                ui.horizontal(|ui| {
                    row_label(ui, "Style", LABEL_W);
                    let r = egui::ComboBox::from_id_salt(("shape-grad-style", id))
                        .selected_text(style.name())
                        .show_ui(ui, |ui| {
                            popup_style(ui);
                            for s in GradientStyle::ALL {
                                if ui.selectable_label(*style == s, s.name()).clicked() {
                                    *style = s;
                                    finished = true;
                                }
                            }
                        })
                        .response;
                    a11y_name(&r, "Gradient style");
                });
                finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
                if check(ui, reverse, "Reverse").changed() {
                    finished = true;
                }
            }
            None => {}
        }

        // Stroke.
        ui.add_space(4.0);
        let mut on = shape.stroke.is_some();
        if check(ui, &mut on, "Stroke").changed() {
            shape.stroke = on.then(|| ShapeStroke {
                color: lin(self.shape.stroke_rgb),
                width: self.shape.stroke_width,
                align: self.shape.align,
                dash: None,
            });
            finished = true;
        }
        if let Some(s) = shape.stroke.as_mut() {
            color_row(ui, "Color", &mut s.color, &mut finished);
            finished |= slider_row_log(ui, "Width", &mut s.width, 0.5..=200.0, " px");
            ui.horizontal(|ui| {
                row_label(ui, "Align", LABEL_W);
                if segmented(
                    ui,
                    &mut s.align,
                    &[
                        (StrokeAlign::Inside, "Inside"),
                        (StrokeAlign::Center, "Center"),
                        (StrokeAlign::Outside, "Outside"),
                    ],
                ) {
                    finished = true;
                }
            });
            let mut dashed = s.dash.is_some();
            if check(ui, &mut dashed, "Dashed")
                .on_hover_text("Dashes and gaps twice the width")
                .changed()
            {
                s.dash = dashed.then_some([2.0, 2.0]);
                finished = true;
            }
        }

        // The geometry's own settings.
        ui.add_space(4.0);
        match &mut shape.geometry {
            ShapeGeometry::Rectangle { rect, radius } => {
                let max = (rect[2].abs().min(rect[3].abs()) / 2.0).max(1.0);
                finished |= slider_row(ui, "Corners", radius, 0.0..=max, " px");
            }
            ShapeGeometry::Polygon { sides, .. } => {
                let mut v = *sides as f32;
                let o = RowOpts {
                    int: true,
                    ..RowOpts::default()
                };
                finished |= slider_row_ex(ui, "Sides", &mut v, 3.0..=24.0, "", o);
                *sides = v.round().clamp(3.0, 24.0) as u32;
            }
            ShapeGeometry::Line {
                weight,
                arrow_start,
                arrow_end,
                ..
            } => {
                finished |= slider_row_log(ui, "Weight", weight, 0.5..=200.0, " px");
                ui.horizontal(|ui| {
                    row_label(ui, "Arrows", LABEL_W);
                    finished |= check(ui, arrow_start, "Start").changed();
                    finished |= check(ui, arrow_end, "End").changed();
                });
            }
            ShapeGeometry::Custom { shape: c, .. } => {
                ui.horizontal(|ui| {
                    row_label(ui, "Shape", LABEL_W);
                    let r = egui::ComboBox::from_id_salt(("shape-custom", id))
                        .selected_text(c.name())
                        .show_ui(ui, |ui| {
                            popup_style(ui);
                            for s in CustomShape::ALL {
                                if ui.selectable_label(*c == s, s.name()).clicked() {
                                    *c = s;
                                    finished = true;
                                }
                            }
                        })
                        .response;
                    a11y_name(&r, "Custom shape");
                });
            }
            ShapeGeometry::Ellipse { .. } | ShapeGeometry::Path { .. } => {}
        }
        if shape != before {
            self.run_coalescing(&SetShape { layer: id, shape }, &format!("shape-{id}"));
        }
        if finished && !crate::color_picker::is_dragging(ui.ctx()) {
            self.editor.end_coalescing();
        }
        ui.horizontal(|ui| {
            if ui
                .small_button("Make work path")
                .on_hover_text("Copy the outline to the pen's work path")
                .clicked()
            {
                self.run(&ShapeToWorkPath { layer: id });
            }
            if ui
                .small_button("Rasterize")
                .on_hover_text("Turn the shape into ordinary pixels")
                .clicked()
            {
                self.run(&RasterizeLayer { layer: id });
            }
        });
        ui.label(
            RichText::new("Stays vector: transforms redraw it crisply.")
                .small()
                .color(MUTED),
        );
    }

    /// Layer ▸ New shape from path: the work path as a shape layer with
    /// the Shape tool's fill and stroke.
    pub(crate) fn shape_from_path(&mut self) {
        let (fill, stroke) = self.shape.paint(self.bg_rgb);
        let fill = fill.or_else(|| {
            stroke.is_none().then(|| Fill::Solid {
                color: lin(self.shape.fill_rgb),
            })
        });
        let new_id = self.editor.doc().next_id();
        self.run(&ShapeFromWorkPath {
            fill,
            stroke,
            above: self.active,
        });
        if self.editor.doc().layer(new_id).is_some() {
            self.set_active(Some(new_id));
        }
    }

    /// Screenshot tokens for the Shape tool (`shape:...`):
    /// `shape:kind=<kind>` picks a kind or custom shape by slug
    /// ("rounded-rectangle", "polygon", "heart", "speech-bubble");
    /// `shape:fill=none|solid|gradient`, `shape:fillhex=RRGGBB`,
    /// `shape:stroke=<width>` (0 turns it off), `shape:strokehex=RRGGBB`,
    /// `shape:align=inside|center|outside`, `shape:radius=<px>`,
    /// `shape:sides=<n>`, `shape:weight=<px>`, `shape:arrows=start|end|both`;
    /// `shape:draw=X0:Y0:X1:Y1` draws one with the current options
    /// (document pixels, as a drag would); `shape:drag=X0:Y0:X1:Y1` shows
    /// the live preview of that drag without committing it;
    /// `shape:frompath` converts the work path; `shape:dash` dashes the
    /// active shape's stroke; `shape:xform=SX:SY:DEG` transforms it.
    pub(crate) fn debug_shape(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("shape:") else {
            return false;
        };
        let slug = |s: &str| s.to_ascii_lowercase().replace(' ', "-");
        let (key, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        let hex = |s: &str| -> Option<[f32; 3]> {
            let v = u32::from_str_radix(s.trim_start_matches('#'), 16).ok()?;
            Some([(v >> 16) & 255, (v >> 8) & 255, v & 255].map(|c| c as f32 / 255.0))
        };
        let st = &mut self.shape;
        match key {
            "kind" => {
                if let Some((k, c, _)) = kind_entries().into_iter().find(|(_, _, n)| slug(n) == arg) {
                    st.params.kind = k;
                    st.params.custom = c;
                }
            }
            "fill" => {
                st.fill = match arg {
                    "none" => FillMode::None,
                    "gradient" => FillMode::Gradient,
                    _ => FillMode::Solid,
                }
            }
            "fillhex" => st.fill_rgb = hex(arg).unwrap_or(st.fill_rgb),
            "strokehex" => st.stroke_rgb = hex(arg).unwrap_or(st.stroke_rgb),
            "stroke" => {
                let w = nums.first().copied().unwrap_or(0.0);
                st.stroke_on = w > 0.0;
                if w > 0.0 {
                    st.stroke_width = w;
                }
            }
            "align" => {
                if let Some(a) = StrokeAlign::ALL.into_iter().find(|a| slug(a.name()) == arg) {
                    st.align = a;
                }
            }
            "radius" => st.params.radius = nums.first().copied().unwrap_or(st.params.radius),
            "sides" => st.params.sides = nums.first().map_or(st.params.sides, |n| *n as u32),
            "weight" => st.params.weight = nums.first().copied().unwrap_or(st.params.weight),
            "arrows" => {
                st.params.arrow_start = matches!(arg, "start" | "both");
                st.params.arrow_end = matches!(arg, "end" | "both");
            }
            "draw" | "drag" if nums.len() == 4 => {
                let (a, b) = ((nums[0], nums[1]), (nums[2], nums[3]));
                if key == "draw" {
                    if let Some((shape, _)) = self.shape.shape_for(a, b, false, false, self.bg_rgb) {
                        self.add_shape(shape);
                    }
                } else {
                    // Flush earlier tokens' redraw first, or it would paint
                    // over the preview later in this frame.
                    if self.dirty {
                        self.refresh(ctx);
                    }
                    self.tool = Tool::Shape;
                    self.drag = Some(DragKind::Shape);
                    self.shape.start = Some(a);
                    self.preview_shape(ctx, a, b);
                }
            }
            "frompath" => self.shape_from_path(),
            // `shape:xform=SX:SY:DEG`: free-transform the active layer about
            // its centre, as committing the on-canvas box would.
            "xform" if nums.len() == 3 => {
                // (A pixel layer works too, for before/after comparisons.)
                let bounds = |l: &Layer| match l.shape_layer() {
                    Some(s) => s.bounds(),
                    None => l
                        .raster_store()?
                        .content_bounds()
                        .map(|b| [b.x as f32, b.y as f32, b.right() as f32, b.bottom() as f32]),
                };
                if let Some((id, [x0, y0, x1, y1])) =
                    self.active_layer().and_then(|l| bounds(l).map(|b| (l.id, b)))
                {
                    let t = Affine::around(
                        (x0 + x1) / 2.0,
                        (y0 + y1) / 2.0,
                        nums[0],
                        nums[1],
                        nums[2].to_radians(),
                    );
                    self.run(&TransformLayer {
                        layer: id,
                        transform: t,
                    });
                }
            }
            "dash" => {
                if let Some((id, mut s)) = self
                    .active_layer()
                    .and_then(|l| l.shape_layer().map(|s| (l.id, s.clone())))
                {
                    if let Some(st) = s.stroke.as_mut() {
                        st.dash = Some([2.0, 2.0]);
                    }
                    self.run(&SetShape { layer: id, shape: s });
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_builds_shapes_from_drags() {
        let mut t = ShapeTool::default();
        // A drag under a pixel across draws nothing.
        assert!(t
            .shape_for((10.0, 10.0), (10.5, 40.0), false, false, [1.0; 3])
            .is_none());
        // Shift + Alt: a square centred on the press point.
        let (s, ends) = t
            .shape_for((50.0, 50.0), (60.0, 55.0), true, true, [1.0; 3])
            .unwrap();
        assert_eq!(ends, ((40.0, 40.0), (60.0, 60.0)));
        assert_eq!(
            s.geometry,
            ShapeGeometry::Rectangle {
                rect: [40.0, 40.0, 20.0, 20.0],
                radius: 0.0
            }
        );
        // The solid fill is the swatch in linear light; no stroke yet.
        match s.fill {
            Some(Fill::Solid { color }) => assert!((color[2] - srgb_to_linear_f(0.92)).abs() < 1e-6),
            ref f => panic!("{f:?}"),
        }
        assert!(s.stroke.is_none());
        // No fill and no stroke: a 1 px hairline keeps it visible.
        t.fill = FillMode::None;
        let (s, _) = t
            .shape_for((0.0, 0.0), (9.0, 9.0), false, false, [1.0; 3])
            .unwrap();
        assert_eq!(s.stroke.map(|s| s.width), Some(1.0));
        // A stroked polygon with six sides.
        t.fill = FillMode::Gradient;
        t.stroke_on = true;
        t.align = StrokeAlign::Outside;
        t.params.kind = ShapeKind::Polygon;
        t.params.sides = 6;
        let (s, _) = t
            .shape_for((0.0, 0.0), (30.0, 30.0), false, false, [1.0; 3])
            .unwrap();
        assert_eq!(
            s.geometry,
            ShapeGeometry::Polygon {
                rect: [0.0, 0.0, 30.0, 30.0],
                sides: 6
            }
        );
        assert!(matches!(s.fill, Some(Fill::Gradient { .. })));
        assert_eq!(
            s.stroke.map(|s| (s.width, s.align)),
            Some((3.0, StrokeAlign::Outside))
        );
    }

    #[test]
    fn kind_entries_list_basic_kinds_then_custom_shapes() {
        let names: Vec<&str> = kind_entries().iter().map(|e| e.2).collect();
        assert_eq!(
            names,
            vec![
                "Rectangle",
                "Rounded Rectangle",
                "Ellipse",
                "Polygon",
                "Line",
                "Star",
                "Arrow",
                "Heart",
                "Speech Bubble"
            ]
        );
    }
}
