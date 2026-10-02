//! The Gradient tool (Shift+G): a multi-stop gradient dragged across the
//! active pixel layer (or its mask while the mask is being edited), or —
//! in fill-layer mode — turned into an editable gradient fill layer.
//!
//! The options bar shows the current gradient as a swatch; clicking it
//! opens a popover with the presets (built-in and the user's, saved in
//! prefs) and the shared stop editor. Next to it: the five styles as
//! drawn icons, blend mode, opacity, Reverse, Dither, Transparency and the
//! Pixels / Fill layer switch. A drag previews the result live on the
//! canvas with a guide line; Shift constrains to 45°. Every commit is one
//! `DrawGradient` or `GradientFillLayer` command.

use super::*;
use crate::adjust_ui::{gradient_editor_opts, paint_gradient};
use crate::options_bar::{bar_slider, Tier};
use lumenply_core::gradient_tool::{
    ramp_position, DrawGradient, GradientFillLayer, GradientPaint, GradientTarget,
};
use lumenply_doc::{Gradient, GradientStop, GradientStyle};
use lumenply_io::srgb_to_linear_f;

/// Where the tool's gradient comes from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GradSource {
    /// Foreground → background colour; follows the colour wells.
    ForeBack,
    /// Foreground → transparent; follows the foreground well.
    ForeClear,
    /// A fixed ramp: a preset, or one edited in the popover.
    Custom(Gradient),
}

/// A gradient the user saved from the popover (kept in prefs.json).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct GradientPreset {
    pub name: String,
    pub gradient: Gradient,
}

impl Default for GradientPreset {
    fn default() -> Self {
        GradientPreset {
            name: "Gradient".into(),
            gradient: Gradient::default(),
        }
    }
}

/// The Gradient tool's options and the drag in progress.
pub(crate) struct GradientTool {
    pub(crate) source: GradSource,
    /// The current gradient's name, as the swatch announces it.
    pub(crate) name: String,
    pub(crate) style: GradientStyle,
    pub(crate) reverse: bool,
    pub(crate) dither: bool,
    pub(crate) transparency: bool,
    /// 0..1.
    pub(crate) opacity: f32,
    pub(crate) blend: BlendMode,
    /// Make a gradient fill layer instead of painting pixels.
    pub(crate) fill_layer: bool,
    /// The gradient popover is open.
    pub(crate) open: bool,
    /// Name typed for "Save as preset".
    pub(crate) preset_name: String,
    /// The drag in progress: press and (constrained) current point,
    /// document space.
    pub(crate) drag: Option<((f32, f32), (f32, f32))>,
    /// Canvas area the last preview painted, to clean up after it.
    pub(crate) last_area: Option<Rect>,
    /// The document area on screen when the drag started.
    pub(crate) view: Option<Rect>,
    /// The press that closed the popover must not also start a drag.
    pub(crate) swallow: bool,
    /// When the last preview finished and how long it took (previews of
    /// big documents are spaced out so the UI stays responsive).
    pub(crate) last_preview: Option<(std::time::Instant, std::time::Duration)>,
}

impl Default for GradientTool {
    fn default() -> Self {
        GradientTool {
            source: GradSource::ForeBack,
            name: FORE_BACK.into(),
            style: GradientStyle::Linear,
            reverse: false,
            dither: true,
            transparency: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            fill_layer: false,
            open: false,
            preset_name: String::new(),
            drag: None,
            last_area: None,
            view: None,
            swallow: false,
            last_preview: None,
        }
    }
}

const FORE_BACK: &str = "Foreground to Background";
const FORE_CLEAR: &str = "Foreground to Transparent";

fn lin(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(srgb_to_linear_f)
}

/// A ramp from 8-bit sRGB stops (position, colour, opacity).
fn ramp(stops: &[(f32, [u8; 3], f32)]) -> Gradient {
    Gradient {
        stops: stops
            .iter()
            .map(|&(p, c, a)| GradientStop {
                alpha: a,
                ..GradientStop::srgb8(p, c)
            })
            .collect(),
    }
}

/// The ramps the popover offers besides the colour-well ones: the six
/// shared with Gradient Map and fill layers, then the tool's own.
pub(crate) fn builtin_presets() -> Vec<(&'static str, Gradient)> {
    let mut v = Gradient::presets();
    v.push((
        "Spectrum",
        ramp(&[
            (0.0, [255, 0, 0], 1.0),
            (1.0 / 6.0, [255, 255, 0], 1.0),
            (2.0 / 6.0, [0, 255, 0], 1.0),
            (0.5, [0, 255, 255], 1.0),
            (4.0 / 6.0, [0, 0, 255], 1.0),
            (5.0 / 6.0, [255, 0, 255], 1.0),
            (1.0, [255, 0, 0], 1.0),
        ]),
    ));
    v.push((
        "Chrome",
        ramp(&[
            (0.0, [28, 92, 160], 1.0),
            (0.46, [214, 236, 250], 1.0),
            (0.5, [255, 255, 255], 1.0),
            (0.52, [92, 66, 30], 1.0),
            (0.66, [196, 150, 88], 1.0),
            (1.0, [250, 246, 236], 1.0),
        ]),
    ));
    v.push((
        "Sunset",
        ramp(&[
            (0.0, [36, 10, 74], 1.0),
            (0.38, [182, 36, 92], 1.0),
            (0.7, [250, 118, 48], 1.0),
            (1.0, [255, 222, 120], 1.0),
        ]),
    ));
    v.push((
        "Transparent Rainbow",
        ramp(&[
            (0.0, [255, 0, 0], 0.0),
            (0.15, [255, 0, 0], 1.0),
            (0.32, [255, 255, 0], 1.0),
            (0.5, [0, 255, 0], 1.0),
            (0.68, [0, 160, 255], 1.0),
            (0.85, [140, 0, 255], 1.0),
            (1.0, [140, 0, 255], 0.0),
        ]),
    ));
    v
}

/// Drag ends with Shift: the end snapped to the nearest 45° from the
/// start, keeping the length.
pub(crate) fn constrain_45(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    let step = std::f32::consts::FRAC_PI_4;
    let ang = (dy.atan2(dx) / step).round() * step;
    (a.0 + len * ang.cos(), a.1 + len * ang.sin())
}

impl GradientTool {
    /// The current ramp (colour wells as sRGB swatches).
    pub(crate) fn gradient(&self, fg: [f32; 3], bg: [f32; 3]) -> Gradient {
        match &self.source {
            GradSource::ForeBack => Gradient::two(lin(fg), lin(bg)),
            GradSource::ForeClear => Gradient {
                stops: vec![
                    GradientStop::new(0.0, lin(fg)),
                    GradientStop {
                        alpha: 0.0,
                        ..GradientStop::new(1.0, lin(fg))
                    },
                ],
            },
            GradSource::Custom(g) => g.clone(),
        }
    }

    /// What a drag from `start` to `end` paints with.
    pub(crate) fn paint(
        &self,
        fg: [f32; 3],
        bg: [f32; 3],
        start: (f32, f32),
        end: (f32, f32),
    ) -> GradientPaint {
        GradientPaint {
            gradient: self.gradient(fg, bg),
            style: self.style,
            start,
            end,
            reverse: self.reverse,
            transparency: self.transparency,
            dither: self.dither,
            opacity: self.opacity.clamp(0.0, 1.0),
            blend: self.blend,
        }
    }

    fn pick(&mut self, name: &str, source: GradSource) {
        self.name = name.to_string();
        self.source = source;
    }
}

/// A style's icon: the style itself, black → white, rendered into a
/// small grid of cells.
fn paint_style_icon(p: &egui::Painter, rect: egui::Rect, style: GradientStyle, ink: Color32) {
    const N: usize = 12;
    let (s, e) = match style {
        GradientStyle::Linear => ((1.0, 6.0), (11.0, 6.0)),
        GradientStyle::Reflected => ((6.0, 6.0), (11.5, 6.0)),
        GradientStyle::Angle => ((6.0, 6.0), (12.0, 6.0)),
        GradientStyle::Radial | GradientStyle::Diamond => ((6.0, 6.0), (12.0, 6.0)),
    };
    let cell = rect.width() / N as f32;
    let mut mesh = egui::Mesh::default();
    for y in 0..N {
        for x in 0..N {
            let t = ramp_position(style, s, e, x as f32 + 0.5, y as f32 + 0.5);
            let k = 0.15 + 0.85 * t;
            let c = Color32::from_rgb(
                (ink.r() as f32 * k) as u8,
                (ink.g() as f32 * k) as u8,
                (ink.b() as f32 * k) as u8,
            );
            let r = egui::Rect::from_min_size(
                rect.min + egui::vec2(x as f32 * cell, y as f32 * cell),
                Vec2::splat(cell),
            );
            mesh.add_colored_rect(r, c);
        }
    }
    p.add(Shape::mesh(mesh));
}

/// The five styles as a strip of icon buttons. Returns true on change.
fn style_buttons(ui: &mut egui::Ui, style: &mut GradientStyle) -> bool {
    let mut changed = false;
    egui::Frame::none()
        .stroke(Stroke::new(1.0, LINE))
        .rounding(RADIUS)
        .inner_margin(egui::Margin::same(2.0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for s in GradientStyle::ALL {
                let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 22.0), Sense::click());
                let on = *style == s;
                r.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::RadioButton,
                        true,
                        on,
                        format!("{} gradient", s.name()),
                    )
                });
                let p = ui.painter();
                if on {
                    p.rect_filled(rect, 4.0, ACCENT_TINT);
                } else if r.hovered() {
                    p.rect_filled(rect, 4.0, HOVER);
                }
                let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat(16.0));
                paint_style_icon(p, icon, s, if on { ACCENT } else { TEXT });
                p.rect_stroke(icon, 1.0, Stroke::new(1.0, if on { ACCENT } else { LINE }));
                focus_ring(ui, &r, rect, 4.0);
                if r.on_hover_text(format!("{} gradient", s.name())).clicked() && !on {
                    *style = s;
                    changed = true;
                }
            }
        });
    changed
}

impl App {
    /// The Gradient tool's options bar.
    pub(crate) fn gradient_options_bar(&mut self, ui: &mut egui::Ui, tier: Tier) {
        // The gradient swatch: click for the presets and stop editor.
        let g = self.gradient.gradient(self.brush_rgb, self.bg_rgb);
        let w = if tier == Tier::Tight { 72.0 } else { 104.0 };
        let (rect, swatch) = ui.allocate_exact_size(egui::vec2(w, 22.0), Sense::click());
        let name = self.gradient.name.clone();
        swatch.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Gradient: {name}. Click to edit"),
            )
        });
        let ramp = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - 16.0, rect.max.y));
        {
            let p = ui.painter();
            paint_gradient(p, ramp.shrink(1.0), &g, true);
            let edge = if self.gradient.open || swatch.hovered() {
                ACCENT
            } else {
                LINE
            };
            p.rect_stroke(rect, 3.0, Stroke::new(1.0, edge));
            p.line_segment(
                [
                    egui::pos2(ramp.max.x, rect.min.y),
                    egui::pos2(ramp.max.x, rect.max.y),
                ],
                Stroke::new(1.0, LINE),
            );
            let c = egui::pos2(rect.max.x - 8.0, rect.center().y);
            p.add(Shape::convex_polygon(
                vec![
                    c + egui::vec2(-3.5, -2.0),
                    c + egui::vec2(3.5, -2.0),
                    c + egui::vec2(0.0, 2.5),
                ],
                MUTED,
                Stroke::NONE,
            ));
        }
        focus_ring(ui, &swatch, rect, 3.0);
        if swatch.clicked() {
            self.gradient.open = !self.gradient.open;
        }
        let swatch = swatch.on_hover_text(format!("{name}: click to edit the gradient"));
        if self.gradient.open {
            self.gradient_popover(ui.ctx(), &swatch);
        }

        style_buttons(ui, &mut self.gradient.style);
        ui.separator();

        let r = egui::ComboBox::from_id_salt("gradient-blend")
            .selected_text(blend_label(self.gradient.blend))
            .width(if tier == Tier::Tight { 78.0 } else { 96.0 })
            .show_ui(ui, |ui| {
                popup_style(ui);
                for m in BlendMode::ALL {
                    ui.selectable_value(&mut self.gradient.blend, m, blend_label(m));
                }
            })
            .response;
        a11y_name(&r, "Gradient blend mode");
        r.on_hover_text("How the gradient blends with the pixels under it");
        let mut op = self.gradient.opacity * 100.0;
        let changed = if tier == Tier::Tight {
            ui.label(RichText::new("Opacity").color(MUTED));
            let r = num_field(
                ui,
                egui::DragValue::new(&mut op)
                    .range(1.0..=100.0)
                    .speed(0.5)
                    .fixed_decimals(0)
                    .suffix("%"),
                54.0,
            );
            a11y_name(&r, "Opacity");
            r.changed()
        } else {
            bar_slider(ui, "Opacity", &mut op, 1.0..=100.0, "%", false)
        };
        if changed {
            self.gradient.opacity = op / 100.0;
        }
        ui.separator();
        check(ui, &mut self.gradient.reverse, "Reverse").on_hover_text("Run the gradient the other way");
        check(ui, &mut self.gradient.dither, "Dither")
            .on_hover_text("Fine noise that keeps smooth ramps from banding in 8-bit exports");
        check(ui, &mut self.gradient.transparency, "Transparency")
            .on_hover_text("Use the stops' opacity; off paints every stop opaque");
        ui.separator();
        segmented(
            ui,
            &mut self.gradient.fill_layer,
            &[(false, "Pixels"), (true, "Fill layer")],
        );
        crate::options_bar::hint_label(ui, tier, Tool::Gradient);
    }

    /// The popover under the gradient swatch: presets, the stop editor,
    /// and saving the current ramp as a preset.
    fn gradient_popover(&mut self, ctx: &egui::Context, swatch: &egui::Response) {
        let id = egui::Id::new("gradient-popover");
        // A colour picker opened from a stop sits outside the popover; a
        // press in it (or the press that closes it) must not close us.
        let picker_was_open = crate::color_picker::is_open(ctx);
        let (fg, bg) = (self.brush_rgb, self.bg_rgb);
        let mut g = self.gradient.gradient(fg, bg);
        let before = g.clone();
        let mut pick: Option<(String, GradSource)> = None;
        let mut delete: Option<usize> = None;
        let mut save = false;
        let user: Vec<GradientPreset> = self.prefs.gradient_presets.clone();
        let current_name = self.gradient.name.clone();
        let area = egui::Area::new(id)
            .kind(egui::UiKind::Popup)
            .order(egui::Order::Foreground)
            .fixed_pos(swatch.rect.left_bottom() + egui::vec2(0.0, 6.0))
            .fade_in(false)
            .sense(BACKDROP_SENSE)
            .constrain_to(ctx.screen_rect().shrink(4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(300.0);
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("GRADIENT").small().strong().color(MUTED));
                            ui.label(RichText::new(&current_name).small().color(TEXT));
                        });
                        // Presets: colour-well ramps, built-ins, then the user's.
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                            let wells = [
                                (FORE_BACK, GradSource::ForeBack),
                                (FORE_CLEAR, GradSource::ForeClear),
                            ];
                            let fixed = builtin_presets()
                                .into_iter()
                                .map(|(n, g)| (n, GradSource::Custom(g)));
                            for (n, src) in wells.into_iter().chain(fixed) {
                                let tmp = GradientTool {
                                    source: src.clone(),
                                    ..GradientTool::default()
                                };
                                if preset_chip(ui, n, &tmp.gradient(fg, bg), n == current_name) {
                                    pick = Some((n.to_string(), src));
                                }
                            }
                            for p in &user {
                                let on = p.name == current_name;
                                if preset_chip(ui, &p.name, &p.gradient, on) {
                                    pick = Some((p.name.clone(), GradSource::Custom(p.gradient.clone())));
                                }
                            }
                        });
                        ui.separator();
                        gradient_editor_opts(ui, 0x6772_6164, &mut g, true, false);
                        ui.separator();
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut self.gradient.preset_name)
                                    .hint_text("Preset name")
                                    .desired_width(150.0),
                            );
                            a11y_name(&r, "Preset name");
                            if ui
                                .button("Save as preset")
                                .on_hover_text("Keep this gradient in the preset grid")
                                .clicked()
                            {
                                save = true;
                            }
                        });
                        if let Some(i) = user.iter().position(|p| p.name == current_name) {
                            if ui
                                .small_button("Delete preset")
                                .on_hover_text(format!("Remove \"{}\" from your presets", current_name))
                                .clicked()
                            {
                                delete = Some(i);
                            }
                        }
                    });
            });
        let popup_rect = area.response.rect;

        if g != before {
            // Editing a stop turns a colour-well ramp into a fixed one.
            self.gradient.source = GradSource::Custom(g.clone());
            if !self.gradient.name.ends_with("(edited)") {
                self.gradient.name = format!("{} (edited)", self.gradient.name);
            }
        }
        if let Some((n, src)) = pick {
            self.gradient.pick(&n, src);
        }
        if save {
            let name = match self.gradient.preset_name.trim() {
                "" => format!("Gradient {}", self.prefs.gradient_presets.len() + 1),
                n => n.to_string(),
            };
            let gradient = self.gradient.gradient(fg, bg);
            self.prefs.gradient_presets.retain(|p| p.name != name);
            self.prefs.gradient_presets.push(GradientPreset {
                name: name.clone(),
                gradient: gradient.clone(),
            });
            self.prefs.save();
            self.gradient.pick(&name, GradSource::Custom(gradient));
            self.gradient.preset_name.clear();
            self.status = format!("Gradient preset \"{name}\" saved");
        }
        if let Some(i) = delete {
            let gone = self.prefs.gradient_presets.remove(i);
            self.prefs.save();
            self.gradient.name = "Custom".into();
            self.status = format!("Gradient preset \"{}\" deleted", gone.name);
        }

        // Esc or a press outside closes it (a press on the canvas is then
        // swallowed rather than starting a gradient).
        if !picker_was_open && !crate::color_picker::is_open(ctx) {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                self.gradient.open = false;
            }
            let press = ctx.input(|i| {
                i.pointer
                    .any_pressed()
                    .then(|| i.pointer.interact_pos())
                    .flatten()
            });
            if let Some(p) = press {
                if !popup_rect.contains(p) && !swatch.rect.contains(p) {
                    self.gradient.open = false;
                    self.gradient.swallow = true;
                }
            }
        }
    }

    /// Canvas input for the Gradient tool: press, drag (live preview),
    /// release (one undo step).
    pub(crate) fn gradient_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if self.gradient.swallow {
            if !ctx.input(|i| i.pointer.any_down()) {
                self.gradient.swallow = false;
            }
            return;
        }
        if resp.drag_started_by(primary) {
            match self.gradient_block() {
                Some(why) => self.status = why.into(),
                None => {
                    if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                        let a = to_doc(p);
                        self.gradient.drag = Some((a, a));
                        self.gradient.last_area = None;
                        self.gradient.last_preview = None;
                        let (x0, y0) = to_doc(resp.rect.min);
                        let (x1, y1) = to_doc(resp.rect.max);
                        self.gradient.view = Some(Rect::new(
                            x0.floor() as i32,
                            y0.floor() as i32,
                            (x1 - x0).ceil().max(1.0) as u32 + 1,
                            (y1 - y0).ceil().max(1.0) as u32 + 1,
                        ));
                        self.drag = Some(DragKind::Gradient);
                    }
                }
            }
        }
        if self.drag == Some(DragKind::Gradient) && resp.dragged_by(primary) {
            if let (Some((a, _)), Some(p)) = (self.gradient.drag, resp.interact_pointer_pos()) {
                let shift = ctx.input(|i| i.modifiers.shift);
                let b = to_doc(p);
                let b = if shift { constrain_45(a, b) } else { b };
                self.gradient.drag = Some((a, b));
                self.preview_gradient(ctx, false);
            }
        }
        if resp.drag_stopped() && self.drag == Some(DragKind::Gradient) {
            self.drag = None;
            let end = resp
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
            let shift = ctx.input(|i| i.modifiers.shift);
            let ends = match (self.gradient.drag.take(), end) {
                (Some((a, _)), Some(p)) => {
                    let b = to_doc(p);
                    Some((a, if shift { constrain_45(a, b) } else { b }))
                }
                _ => None,
            };
            // Under two screen pixels is a click, not a drag.
            let long = |(a, b): ((f32, f32), (f32, f32))| (b.0 - a.0).hypot(b.1 - a.1) * self.zoom >= 2.0;
            match ends.filter(|e| long(*e)) {
                Some((a, b)) => self.commit_gradient(a, b),
                None => {
                    let area = self.gradient.last_area.take();
                    self.mark(area);
                }
            }
            self.gradient.last_area = None;
        } else if resp.clicked_by(primary) {
            self.status = "Drag to draw a gradient: Shift constrains to 45°".into();
        }
    }

    /// Why a drag can't draw right now, if it can't.
    fn gradient_block(&self) -> Option<&'static str> {
        if self.gradient.fill_layer {
            return None;
        }
        let Some(layer) = self.active_layer() else {
            return Some("Select a layer for the gradient");
        };
        if self.editing_mask {
            return layer
                .mask
                .is_none()
                .then_some("This layer has no mask to draw on");
        }
        if layer.pixels().is_none() {
            return Some("Select a pixel layer for the gradient, or switch the tool to Fill layer");
        }
        None
    }

    /// The command a drag from `a` to `b` commits.
    fn gradient_tool_command(&self, a: (f32, f32), b: (f32, f32)) -> Option<Box<dyn Command>> {
        let paint = self.gradient.paint(self.brush_rgb, self.bg_rgb, a, b);
        if self.gradient.fill_layer {
            return Some(Box::new(GradientFillLayer {
                paint,
                above: self.active,
            }));
        }
        let layer = self.active?;
        Some(Box::new(DrawGradient {
            layer,
            target: if self.editing_mask {
                GradientTarget::Mask
            } else {
                GradientTarget::Pixels
            },
            paint,
        }))
    }

    fn commit_gradient(&mut self, a: (f32, f32), b: (f32, f32)) {
        let Some(cmd) = self.gradient_tool_command(a, b) else {
            return;
        };
        let new_id = self.editor.doc().next_id();
        self.run(cmd.as_ref());
        if self.gradient.fill_layer && self.editor.doc().layer(new_id).is_some() {
            self.set_active(Some(new_id));
        }
    }

    /// Paint the drag's result over the canvas: the real command on a copy
    /// of the document, recomposited over the part of the canvas on screen.
    /// Previews that take long are spaced out (`force` skips that).
    pub(crate) fn preview_gradient(&mut self, ctx: &egui::Context, force: bool) {
        let Some((a, b)) = self.gradient.drag else { return };
        if let Some((at, took)) = self.gradient.last_preview {
            if !force && at.elapsed() < took.saturating_mul(2).min(std::time::Duration::from_millis(400)) {
                ctx.request_repaint();
                return;
            }
        }
        let t0 = std::time::Instant::now();
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let Some(cmd) = self.gradient_tool_command(a, b) else {
            return;
        };
        let mut preview = doc.clone();
        let mut area = cmd.affected(doc).unwrap_or(canvas);
        if let Some(v) = self.gradient.view {
            area = area.intersect(&v);
        }
        if (b.0 - a.0).hypot(b.1 - a.1) < 1e-3 || cmd.apply(&mut preview).is_err() {
            return;
        }
        let paint = match self.gradient.last_area {
            Some(prev) => area.union(&prev),
            None => area,
        };
        self.preview(ctx, &preview, Some(paint));
        self.gradient.last_area = Some(area);
        self.gradient.last_preview = Some((std::time::Instant::now(), t0.elapsed()));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        self.status = format!(
            "{} gradient {:.0} px at {:.0}°",
            self.gradient.style.name(),
            dx.hypot(dy),
            // `+ 0.0` turns −0 into 0.
            (-dy).atan2(dx).to_degrees().round() + 0.0
        );
    }

    /// The guide line with its start and end handles, over the preview.
    pub(crate) fn paint_gradient_overlay(&self, painter: &egui::Painter, resp: &egui::Response) {
        if self.drag != Some(DragKind::Gradient) {
            return;
        }
        let Some((a, b)) = self.gradient.drag else { return };
        let origin = resp.rect.min + self.pan;
        let ts = |(x, y): (f32, f32)| egui::pos2(origin.x + x * self.zoom, origin.y + y * self.zoom);
        let (pa, pb) = (ts(a), ts(b));
        painter.line_segment([pa, pb], Stroke::new(3.0, Color32::from_black_alpha(140)));
        painter.line_segment([pa, pb], Stroke::new(1.0, Color32::WHITE));
        for (p, filled) in [(pa, true), (pb, false)] {
            painter.circle_filled(p, 5.5, Color32::from_black_alpha(140));
            if filled {
                painter.circle_filled(p, 4.0, Color32::WHITE);
            } else {
                painter.circle_filled(p, 4.0, ACCENT);
                painter.circle_stroke(p, 4.0, Stroke::new(1.5, Color32::WHITE));
            }
        }
    }

    /// Screenshot tokens for the Gradient tool (`gradient:...`):
    /// `gradient:open` opens the popover; `gradient:preset=<n>` picks
    /// preset `n` (0 = foreground → background, 1 = foreground →
    /// transparent, then the built-ins); `gradient:style=<name>`;
    /// `gradient:blend=<mode>`; `gradient:opacity=<percent>`;
    /// `gradient:reverse`, `gradient:nodither`, `gradient:fill-layer`;
    /// `gradient:drag=X0:Y0:X1:Y1` previews a drag (document pixels)
    /// without committing it; `gradient:draw=X0:Y0:X1:Y1` commits one.
    pub(crate) fn debug_gradient(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("gradient:") else {
            return false;
        };
        let (key, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        let slug = |s: &str| s.to_ascii_lowercase().replace(' ', "-");
        match key {
            "open" => {
                self.tool = Tool::Gradient;
                self.gradient.open = true;
            }
            "preset" => {
                let n = nums.first().copied().unwrap_or(0.0) as usize;
                match n {
                    0 => self.gradient.pick(FORE_BACK, GradSource::ForeBack),
                    1 => self.gradient.pick(FORE_CLEAR, GradSource::ForeClear),
                    _ => {
                        if let Some((name, g)) = builtin_presets().into_iter().nth(n - 2) {
                            self.gradient.pick(name, GradSource::Custom(g));
                        }
                    }
                }
            }
            "style" => {
                if let Some(s) = GradientStyle::ALL.into_iter().find(|s| slug(s.name()) == arg) {
                    self.gradient.style = s;
                }
            }
            "blend" => {
                if let Some(m) = BlendMode::ALL.into_iter().find(|m| slug(m.name()) == arg) {
                    self.gradient.blend = m;
                }
            }
            "opacity" => self.gradient.opacity = nums.first().map_or(1.0, |v| v / 100.0),
            "reverse" => self.gradient.reverse = true,
            "nodither" => self.gradient.dither = false,
            "fill-layer" => self.gradient.fill_layer = true,
            "draw" | "drag" if nums.len() == 4 => {
                let (a, b) = ((nums[0], nums[1]), (nums[2], nums[3]));
                self.tool = Tool::Gradient;
                if key == "draw" {
                    self.commit_gradient(a, b);
                } else {
                    // Flush earlier tokens' redraw first, or it would paint
                    // over the preview later in this frame.
                    if self.dirty {
                        self.refresh(ctx);
                    }
                    self.drag = Some(DragKind::Gradient);
                    self.gradient.drag = Some((a, b));
                    self.gradient.view = None;
                    self.preview_gradient(ctx, true);
                    if let Some((_, took)) = self.gradient.last_preview {
                        eprintln!("gradient preview took {:.1} ms", took.as_secs_f64() * 1000.0);
                    }
                }
            }
            _ => return false,
        }
        true
    }
}

/// A preset chip in the popover's grid. Returns true when clicked.
fn preset_chip(ui: &mut egui::Ui, name: &str, g: &Gradient, on: bool) -> bool {
    let (r, chip) = ui.allocate_exact_size(egui::vec2(40.0, 22.0), Sense::click());
    chip.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            true,
            on,
            format!("Gradient preset: {name}"),
        )
    });
    let p = ui.painter();
    paint_gradient(p, r.shrink(1.0), g, true);
    let edge = if on {
        Stroke::new(2.0, ACCENT)
    } else if chip.hovered() || chip.has_focus() {
        Stroke::new(1.0, ACCENT)
    } else {
        Stroke::new(1.0, LINE)
    };
    p.rect_stroke(r, 3.0, edge);
    chip.on_hover_text(name).clicked()
}

/// A blend mode's name for menus ("normal" → "Normal", "hard-light" →
/// "Hard Light").
fn blend_label(m: BlendMode) -> String {
    m.name()
        .split(['-', '_', ' '])
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_resolves_its_gradient_from_the_colour_wells() {
        let mut t = GradientTool::default();
        assert_eq!(t.name, "Foreground to Background");
        assert!(t.dither, "dither is on by default, as in Photoshop");
        let fg = [1.0, 0.0, 0.0];
        let bg = [0.0, 0.0, 1.0];
        let g = t.gradient(fg, bg);
        assert_eq!(g.stops.len(), 2);
        assert_eq!(
            (g.stops[0].color, g.stops[1].color),
            (fg, bg),
            "sRGB 0/1 are linear 0/1"
        );
        // Mid-grey well: stored linear (sRGB 0.5 → 0.2140).
        let g = t.gradient([0.5; 3], bg);
        assert!((g.stops[0].color[0] - 0.2140).abs() < 1e-4);
        t.source = GradSource::ForeClear;
        let g = t.gradient(fg, bg);
        assert_eq!(
            (g.stops[0].alpha, g.stops[1].alpha, g.stops[1].color),
            (1.0, 0.0, fg)
        );
        let p = t.paint(fg, bg, (0.0, 0.0), (10.0, 0.0));
        assert_eq!(
            (p.opacity, p.blend, p.transparency),
            (1.0, BlendMode::Normal, true)
        );
    }

    #[test]
    fn shift_snaps_the_drag_to_45_degrees() {
        let (x, y) = constrain_45((0.0, 0.0), (10.0, 1.0));
        assert!((x - 10.05).abs() < 0.01 && y.abs() < 1e-4, "{x} {y}");
        let (x, y) = constrain_45((10.0, 10.0), (20.0, 19.0));
        let r = 10.0f32.hypot(9.0) / 2f32.sqrt();
        assert!((x - (10.0 + r)).abs() < 1e-3 && (y - (10.0 + r)).abs() < 1e-3);
        let (x, y) = constrain_45((0.0, 0.0), (-1.0, -30.0));
        assert!(x.abs() < 1e-4 && (y + 30.017).abs() < 1e-3, "{x} {y}");
    }

    #[test]
    fn presets_cover_the_shared_six_and_the_tool_extras() {
        let names: Vec<&str> = builtin_presets().iter().map(|p| p.0).collect();
        assert_eq!(
            names,
            vec![
                "Black, White",
                "Sepia",
                "Cyanotype",
                "Teal, Orange",
                "Violet, Gold",
                "Copper",
                "Spectrum",
                "Chrome",
                "Sunset",
                "Transparent Rainbow"
            ]
        );
        assert_eq!(blend_label(BlendMode::HardLight), "Hard Light");
        assert_eq!(blend_label(BlendMode::Normal), "Normal");
        // Spectrum's sixth stop is magenta at 5/6.
        let s = &builtin_presets()[6].1;
        assert_eq!(s.stops[5].color, [1.0, 0.0, 1.0]);
    }

    /// The app on a small blank document, with no dialog and no autosave.
    fn small_app() -> App {
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(blank(64, 48), None);
        app
    }

    #[test]
    fn the_popover_and_bar_name_every_control() {
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        app.tool = Tool::Gradient;
        app.gradient.open = true;
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.gradient.open, "the popover stayed open");
        assert_eq!(missing, Vec::<String>::new());
    }

    #[test]
    fn a_drag_paints_one_undo_step_and_fill_mode_adds_a_layer() {
        let mut app = small_app();
        let ctx = crate::a11y_tests::ctx();
        let bg = app.editor.doc().layers().last().unwrap().id;
        app.set_active(Some(bg));
        let steps = app.editor.history().len();
        app.brush_rgb = [0.0; 3];
        app.bg_rgb = [1.0; 3];
        app.gradient.dither = false;
        assert!(app.debug_gradient(&ctx, "gradient:draw=0:0:64:0"));
        assert_eq!(app.editor.history().len(), steps + 1);
        let px = app
            .editor
            .doc()
            .layer(bg)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(31, 10);
        // Pixel 31's centre (31.5 of 64) is at t = 0.4922: sRGB 0.4922 →
        // linear 0.2069 (stored at 16 bits).
        assert!((px.r - 0.2069).abs() < 1e-3, "{px:?}");
        // Fill-layer mode: a new active gradient fill layer above.
        app.debug_gradient(&ctx, "gradient:fill-layer");
        app.debug_gradient(&ctx, "gradient:style=radial");
        app.debug_gradient(&ctx, "gradient:draw=32:24:32:4");
        let top = app.editor.doc().layers().last().unwrap();
        assert!(top.fill_layer().is_some());
        assert_eq!(app.active, Some(top.id));
        assert_eq!(app.editor.history().len(), steps + 2);
    }
}
