//! Properties sections for Gradient Map, Channel Mixer, Photo Filter and
//! Selective Color, and the gradient stop editor they share with gradient
//! fill layers. Per-panel UI state (selected stop, output channel, colour
//! family) lives in egui's memory, keyed by layer, so `App` stays as is.

use super::*;
use crate::color_picker::{format_hex, paint_swatch, picker_popup, Placement};
use lumenply_doc::adjust::SELECTIVE_FAMILIES;
use lumenply_doc::{Fill, Gradient, GradientStop, GradientStyle};
use lumenply_io::{linear_to_srgb_f, srgb_to_linear_f};

/// What an editor did this frame: `changed` edits the document (coalesced),
/// `finished` closes the undo step.
#[derive(Default, Clone, Copy)]
pub(crate) struct Edit {
    pub changed: bool,
    pub finished: bool,
}

impl std::ops::BitOrAssign for Edit {
    fn bitor_assign(&mut self, o: Edit) {
        self.changed |= o.changed;
        self.finished |= o.finished;
    }
}

#[derive(Clone, Default)]
struct GradState {
    /// Index (into `stops`) of the stop the row below edits.
    selected: usize,
    /// The stop being dragged.
    drag: Option<usize>,
    /// The drag has left the strip far enough to remove the stop on release.
    pulled_off: bool,
}

/// Gamma sRGB (+ alpha) → an egui colour.
fn c32(c: [f32; 4]) -> Color32 {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(u(c[0]), u(c[1]), u(c[2]), u(c[3]))
}

/// Paint `g` across `rect` (with a checkerboard under it when `alpha`).
pub(crate) fn paint_gradient(p: &egui::Painter, rect: egui::Rect, g: &Gradient, alpha: bool) {
    if alpha {
        let s = 6.0;
        let (nx, ny) = (
            (rect.width() / s).ceil() as i32,
            (rect.height() / s).ceil() as i32,
        );
        for y in 0..ny {
            for x in 0..nx {
                let c = if (x + y) % 2 == 0 {
                    Color32::from_gray(150)
                } else {
                    Color32::from_gray(100)
                };
                let r = egui::Rect::from_min_size(
                    rect.min + egui::vec2(x as f32 * s, y as f32 * s),
                    Vec2::splat(s),
                )
                .intersect(rect);
                p.rect_filled(r, 0.0, c);
            }
        }
    }
    let n = 48;
    let samples = g.sample_gamma(n + 1);
    let mut mesh = egui::Mesh::default();
    for (i, c) in samples.iter().enumerate() {
        let x = rect.left() + rect.width() * i as f32 / n as f32;
        let col = c32(if alpha { *c } else { [c[0], c[1], c[2], 1.0] });
        mesh.colored_vertex(egui::pos2(x, rect.top()), col);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), col);
        if i > 0 {
            let k = (i as u32) * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    p.add(Shape::mesh(mesh));
}

/// The gradient stop editor: a strip with draggable stops below it. Click
/// the strip to add a stop, drag a stop to move it, drag it off the strip
/// to remove it, click a stop to edit its colour. Below: the selected
/// stop's colour, location (and opacity when `alpha`), and presets.
pub(crate) fn gradient_editor(ui: &mut egui::Ui, salt: u64, g: &mut Gradient, alpha: bool) -> Edit {
    gradient_editor_opts(ui, salt, g, alpha, true)
}

/// [`gradient_editor`], with the preset row only when `presets` (the
/// Gradient tool's popover shows its own preset grid).
pub(crate) fn gradient_editor_opts(
    ui: &mut egui::Ui,
    salt: u64,
    g: &mut Gradient,
    alpha: bool,
    presets: bool,
) -> Edit {
    let id = egui::Id::new(("gradient-editor", salt));
    let mut st: GradState = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut out = Edit::default();
    if g.stops.len() < 2 {
        *g = Gradient { stops: g.sorted() };
        if g.stops.len() < 2 {
            let c = g.stops[0].color;
            g.stops.push(GradientStop::new(1.0, c));
        }
        out.changed = true;
    }
    let inset = 7.0;
    let (strip_h, marker_h) = (24.0, 16.0);
    let w = ui.available_width().max(120.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(w, strip_h + marker_h + 2.0), Sense::click_and_drag());
    let n = g.stops.len();
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            format!("Gradient, {n} stops: click to add a stop, drag a stop away to remove it"),
        )
    });
    let strip = egui::Rect::from_min_size(
        rect.min + egui::vec2(inset, 0.0),
        egui::vec2(rect.width() - 2.0 * inset, strip_h),
    );
    let x_of = |pos: f32| strip.left() + pos.clamp(0.0, 1.0) * strip.width();
    let pos_of = |x: f32| ((x - strip.left()) / strip.width()).clamp(0.0, 1.0);
    let band_top = strip.bottom() + 2.0;
    // The stop under a pointer: markers in the band below the strip.
    let hit = |stops: &[GradientStop], q: Pos2| -> Option<usize> {
        if q.y < band_top - 4.0 || q.y > rect.bottom() + 4.0 {
            return None;
        }
        stops
            .iter()
            .enumerate()
            .map(|(i, s)| (i, (x_of(s.pos) - q.x).abs()))
            .filter(|(_, d)| *d <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    let mut open_picker = false;
    if resp.drag_started() {
        if let Some(q) = ui.input(|i| i.pointer.press_origin()) {
            st.drag = hit(&g.stops, q);
            if let Some(i) = st.drag {
                st.selected = i;
            }
            st.pulled_off = false;
        }
    }
    if resp.dragged() {
        if let (Some(i), Some(q)) = (st.drag, resp.interact_pointer_pos()) {
            if i < g.stops.len() {
                let p = pos_of(q.x);
                if (g.stops[i].pos - p).abs() > 1e-6 {
                    g.stops[i].pos = p;
                    out.changed = true;
                }
                st.pulled_off = g.stops.len() > 2 && (q.y > rect.bottom() + 24.0 || q.y < rect.top() - 24.0);
            }
        }
    }
    if resp.drag_stopped() {
        if let Some(i) = st.drag.take() {
            if st.pulled_off && i < g.stops.len() && g.stops.len() > 2 {
                g.stops.remove(i);
                out.changed = true;
            }
            out.finished = true;
        }
        st.pulled_off = false;
    }
    if resp.clicked() {
        if let Some(q) = resp.interact_pointer_pos() {
            match hit(&g.stops, q) {
                Some(i) => {
                    st.selected = i;
                    open_picker = true;
                }
                None if g.stops.len() < 32 => {
                    let pos = pos_of(q.x);
                    let c = g.eval_gamma(pos);
                    g.stops.push(GradientStop {
                        pos,
                        color: [c[0], c[1], c[2]].map(srgb_to_linear_f),
                        alpha: c[3],
                    });
                    st.selected = g.stops.len() - 1;
                    out.changed = true;
                    out.finished = true;
                }
                None => {}
            }
        }
    }
    st.selected = st.selected.min(g.stops.len() - 1);

    // Paint: the strip, then a marker per stop.
    let p = ui.painter_at(rect.expand(2.0));
    paint_gradient(&p, strip, g, alpha);
    p.rect_stroke(strip, 2.0, Stroke::new(1.0, LINE));
    focus_ring(ui, &resp, strip, 2.0);
    for (i, s) in g.stops.iter().enumerate() {
        let x = x_of(s.pos);
        let gone = st.drag == Some(i) && st.pulled_off;
        let top = band_top;
        let pts = vec![
            egui::pos2(x, top),
            egui::pos2(x + 6.0, top + 5.0),
            egui::pos2(x + 6.0, top + marker_h - 1.0),
            egui::pos2(x - 6.0, top + marker_h - 1.0),
            egui::pos2(x - 6.0, top + 5.0),
        ];
        let rgb = s.color.map(linear_to_srgb_f);
        let fill = c32([rgb[0], rgb[1], rgb[2], if gone { 0.3 } else { 1.0 }]);
        let edge = if i == st.selected { ACCENT } else { MUTED };
        p.add(Shape::convex_polygon(
            pts,
            fill,
            Stroke::new(if i == st.selected { 2.0 } else { 1.0 }, edge),
        ));
    }
    if let Some(q) = resp.hover_pos() {
        if hit(&g.stops, q).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    // The selected stop's own settings.
    let sel = st.selected;
    let swatch_id = id.with("swatch");
    ui.horizontal(|ui| {
        row_label(ui, "Stop color", LABEL_W);
        let (r, _) = ui.allocate_exact_size(egui::vec2(34.0, 20.0), Sense::hover());
        let sresp = ui.interact(r, swatch_id, Sense::click());
        let mut srgb = g.stops[sel].color.map(linear_to_srgb_f);
        let hex = format_hex(srgb);
        sresp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::ColorButton, true, format!("Stop colour {hex}"))
        });
        let popup = swatch_id.with("color-picker");
        if open_picker {
            ui.memory_mut(|m| m.open_popup(popup));
        }
        let open = ui.memory(|m| m.is_popup_open(popup));
        paint_swatch(
            ui.painter(),
            r,
            srgb,
            if open {
                ACCENT
            } else if sresp.hovered() {
                MUTED
            } else {
                LINE
            },
        );
        let sresp = if open {
            sresp
        } else {
            sresp.on_hover_text(format!("{hex}  ·  click to edit"))
        };
        let e = picker_popup(ui, &sresp, popup, Some("Stop colour"), &mut srgb, Placement::Auto);
        if e.changed {
            g.stops[sel].color = srgb.map(srgb_to_linear_f);
            out.changed = true;
        }
        out.finished |= e.drag_stopped || (e.changed && !e.dragging);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can = g.stops.len() > 2;
            let rm = ui
                .add_enabled(can, egui::Button::new(RichText::new("Remove").small()))
                .on_hover_text("Remove this stop")
                .on_disabled_hover_text("A gradient needs at least two stops");
            if rm.clicked() {
                g.stops.remove(sel);
                st.selected = 0;
                out.changed = true;
                out.finished = true;
            }
        });
    });
    let sel = st.selected.min(g.stops.len() - 1);
    out.finished |= slider_row_scaled(ui, "Location", &mut g.stops[sel].pos, 0.0..=1.0, 100.0, "%");
    if alpha {
        out.finished |= slider_row_scaled(ui, "Stop opacity", &mut g.stops[sel].alpha, 0.0..=1.0, 100.0, "%");
    }

    if !presets {
        ui.data_mut(|d| d.insert_temp(id, st));
        return out;
    }
    // Presets: a small strip per ramp.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (name, preset) in Gradient::presets() {
            let (r, chip) = ui.allocate_exact_size(egui::vec2(40.0, 16.0), Sense::click());
            chip.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Gradient preset: {name}"))
            });
            let p = ui.painter();
            paint_gradient(p, r, &preset, false);
            let edge = if chip.hovered() || chip.has_focus() {
                ACCENT
            } else {
                LINE
            };
            p.rect_stroke(r, 2.0, Stroke::new(1.0, edge));
            if chip.on_hover_text(name).clicked() {
                *g = preset;
                st.selected = 0;
                out.changed = true;
                out.finished = true;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, st));
    out
}

impl App {
    /// Layer ▸ New fill layer: a solid fill in the foreground colour, or a
    /// foreground → background gradient, above the active layer (masked
    /// by the selection, if any).
    pub(crate) fn add_fill_layer(&mut self, gradient: bool) {
        // The swatches hold sRGB; fills store linear light.
        let (fg, bg) = (
            self.brush_rgb.map(srgb_to_linear_f),
            self.bg_rgb.map(srgb_to_linear_f),
        );
        let fill = if gradient {
            Fill::gradient(Gradient::two(fg, bg))
        } else {
            Fill::Solid { color: fg }
        };
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddFillLayer::new(fill);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    /// Properties of a fill layer: its kind, then the colour or the
    /// gradient with its style, angle, scale and direction.
    pub(crate) fn fill_ui(&mut self, ui: &mut egui::Ui, id: LayerId, mut fill: Fill) {
        let before = fill.clone();
        let mut finished = false;
        ui.horizontal(|ui| {
            row_label(ui, "Fill", LABEL_W);
            // 0 solid, 1 gradient, 2 pattern.
            let kind = |f: &Fill| match f {
                Fill::Solid { .. } => 0u8,
                Fill::Gradient { .. } => 1,
                Fill::Pattern { .. } => 2,
            };
            let mut k = kind(&fill);
            if segmented(ui, &mut k, &[(0, "Solid color"), (1, "Gradient"), (2, "Pattern")]) {
                let first = match &fill {
                    Fill::Solid { color } => *color,
                    Fill::Gradient { gradient, .. } => gradient.sorted()[0].color,
                    Fill::Pattern { .. } => [0.5; 3],
                };
                fill = match k {
                    0 => Fill::Solid { color: first },
                    1 => Fill::gradient(Gradient::two(first, [1.0; 3])),
                    _ => Fill::pattern(
                        self.editor
                            .doc()
                            .patterns
                            .first()
                            .cloned()
                            .unwrap_or_else(|| self.patterns.default_pattern())
                            .reference(),
                    ),
                };
                finished = true;
            }
        });
        match &mut fill {
            Fill::Solid { color } => {
                ui.horizontal(|ui| {
                    row_label(ui, "Color", LABEL_W);
                    let mut srgb = color.map(linear_to_srgb_f);
                    let r = crate::color_picker::color_edit_button_rgb(ui, &mut srgb);
                    if r.changed() {
                        *color = srgb.map(srgb_to_linear_f);
                    }
                    finished |= r.drag_stopped() || (r.changed() && !r.dragged());
                });
            }
            Fill::Gradient {
                gradient,
                style,
                angle,
                scale,
                reverse,
                offset: _,
            } => {
                finished |= gradient_editor(ui, id, gradient, true).finished;
                let current = style.name();
                combo_row(ui, "Style", ("fill-style", id), current, |ui| {
                    for s in GradientStyle::ALL {
                        if ui.selectable_label(*style == s, s.name()).clicked() {
                            *style = s;
                            finished = true;
                        }
                    }
                });
                finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
                finished |= slider_row_scaled(ui, "Scale", scale, 0.1..=1.5, 100.0, "%");
                if check(ui, reverse, "Reverse").changed() {
                    finished = true;
                }
            }
            Fill::Pattern { .. } => {
                finished |= self.pattern_fill_rows(ui, crate::pattern_ui::PickTarget::Fill(id), &mut fill);
            }
        }
        if fill != before {
            self.run_coalescing(&SetFill { layer: id, fill }, &format!("fill-{id}"));
        }
        if finished && !crate::color_picker::is_dragging(ui.ctx()) {
            self.editor.end_coalescing();
        }
        ui.label(
            RichText::new("Covers the canvas; mask it to shape it.")
                .small()
                .color(MUTED),
        );
    }

    /// Screenshot tokens for this area (`adj:...`): `adj:add=<name>` adds
    /// the adjustment preset of that name (slugged: `gradient-map`) above
    /// the active layer; `adj:gradient=<n>` sets the active gradient map
    /// to gradient preset `n`; `adj:mixer=<n>` applies channel-mixer
    /// preset `n`; `adj:filter=<n>` picks photo filter `n` at 60%;
    /// `adj:selc` sets a sample Selective Color (reds toward orange,
    /// blues deeper).
    pub(crate) fn debug_adjust(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("adj:") else {
            return false;
        };
        let slug = |s: &str| s.to_ascii_lowercase().replace([' ', '/', '&'], "-");
        let (key, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let n: usize = arg.parse().unwrap_or(0);
        let current = self
            .active
            .and_then(|id| match &self.editor.doc().layer(id)?.content {
                LayerContent::Adjustment(a) => Some((id, a.clone())),
                _ => None,
            });
        let set = |app: &mut App, layer: LayerId, adjustment: Adjustment| {
            app.run(&SetAdjustment { layer, adjustment });
        };
        // Fill layers: `adj:fill=solid|gradient` adds one;
        // `adj:fillgrad=<n>` gives the active gradient fill preset `n`,
        // `adj:fillstyle=<style>` its style, `adj:fillfade` fades its end
        // stop to transparent.
        let fill_now = self
            .active_layer()
            .and_then(|l| l.fill_layer())
            .map(|f| f.fill.clone());
        let set_fill = |app: &mut App, f: Fill| {
            if let Some(layer) = app.active {
                app.run(&SetFill { layer, fill: f });
            }
        };
        match (key, fill_now) {
            ("fill", _) => {
                self.add_fill_layer(arg == "gradient");
                return true;
            }
            (
                "fillgrad",
                Some(Fill::Gradient {
                    style,
                    angle,
                    scale,
                    reverse,
                    offset,
                    ..
                }),
            ) => {
                if let Some((_, gradient)) = Gradient::presets().into_iter().nth(n) {
                    set_fill(
                        self,
                        Fill::Gradient {
                            gradient,
                            style,
                            angle,
                            scale,
                            reverse,
                            offset,
                        },
                    );
                }
                return true;
            }
            (
                "fillstyle",
                Some(Fill::Gradient {
                    gradient,
                    angle,
                    scale,
                    reverse,
                    offset,
                    ..
                }),
            ) => {
                if let Some(style) = GradientStyle::ALL.into_iter().find(|s| slug(s.name()) == arg) {
                    set_fill(
                        self,
                        Fill::Gradient {
                            gradient,
                            style,
                            angle,
                            scale,
                            reverse,
                            offset,
                        },
                    );
                }
                return true;
            }
            (
                "fillfade",
                Some(Fill::Gradient {
                    mut gradient,
                    style,
                    angle,
                    scale,
                    reverse,
                    offset,
                }),
            ) => {
                if let Some(s) = gradient.stops.iter_mut().max_by(|a, b| a.pos.total_cmp(&b.pos)) {
                    s.alpha = 0.0;
                }
                set_fill(
                    self,
                    Fill::Gradient {
                        gradient,
                        style,
                        angle,
                        scale,
                        reverse,
                        offset,
                    },
                );
                return true;
            }
            _ => {}
        }
        match (key, current) {
            ("add", _) => {
                if let Some((_, a)) = adjustment_presets()
                    .into_iter()
                    .find(|(name, _)| slug(name) == arg)
                {
                    self.add_adjustment(a);
                }
            }
            ("gradient", Some((id, Adjustment::GradientMap { reverse, .. }))) => {
                if let Some((_, g)) = Gradient::presets().into_iter().nth(n) {
                    set(self, id, Adjustment::GradientMap { gradient: g, reverse });
                }
            }
            ("mixer", Some((id, Adjustment::ChannelMixer { red, green, blue, .. }))) => {
                if let Some((_, Some(gray))) = mixer_presets().into_iter().nth(n) {
                    let monochrome = true;
                    set(
                        self,
                        id,
                        Adjustment::ChannelMixer {
                            red,
                            green,
                            blue,
                            monochrome,
                            gray,
                        },
                    );
                }
            }
            (
                "filter",
                Some((
                    id,
                    Adjustment::PhotoFilter {
                        preserve_luminosity, ..
                    },
                )),
            ) => {
                if let Some((_, c)) = PHOTO_FILTERS.get(n) {
                    let color = c.map(|v| srgb_to_linear_f(v as f32 / 255.0));
                    let density = 0.6;
                    set(
                        self,
                        id,
                        Adjustment::PhotoFilter {
                            color,
                            density,
                            preserve_luminosity,
                        },
                    );
                }
            }
            ("selc", Some((id, Adjustment::SelectiveColor { absolute, .. }))) => {
                let mut colors = [[0.0; 4]; 9];
                colors[0] = [-0.3, 0.2, 0.6, 0.0];
                colors[4] = [0.4, 0.1, -0.2, 0.3];
                colors[6] = [0.0, 0.0, 0.15, 0.0];
                set(self, id, Adjustment::SelectiveColor { colors, absolute });
            }
            _ => return false,
        }
        true
    }
}

/// A labelled dropdown row; `add` fills the list.
fn combo_row(
    ui: &mut egui::Ui,
    label: &str,
    salt: (&str, LayerId),
    text: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        row_label(ui, label, LABEL_W);
        ui.spacing_mut().combo_width = ui.available_width();
        let r = egui::ComboBox::from_id_salt(salt)
            .selected_text(text)
            .show_ui(ui, |ui| {
                popup_style(ui);
                add(ui);
            });
        a11y_name(&r.response, label);
    });
}

/// Channel Mixer presets: `(name, monochrome, gray row)`; `None` resets to
/// the identity mix.
fn mixer_presets() -> Vec<(&'static str, Option<[f32; 4]>)> {
    vec![
        ("Default (no change)", None),
        ("B&W with red filter", Some([1.0, 0.0, 0.0, 0.0])),
        ("B&W with orange filter", Some([0.5, 0.5, 0.0, 0.0])),
        ("B&W with yellow filter", Some([0.34, 0.66, 0.0, 0.0])),
        ("B&W with green filter", Some([0.21, 0.7, 0.09, 0.0])),
        ("B&W with blue filter", Some([0.0, 0.0, 1.0, 0.0])),
        ("B&W infrared", Some([-0.7, 2.0, -0.3, 0.0])),
    ]
}

/// The Channel Mixer preset the settings match, or "Custom" (Photoshop's
/// Preset menu shows it the same way).
fn mixer_preset_name(
    red: &[f32; 4],
    green: &[f32; 4],
    blue: &[f32; 4],
    monochrome: bool,
    gray: &[f32; 4],
) -> &'static str {
    let same = |a: &[f32; 4], b: &[f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
    let identity = same(red, &[1.0, 0.0, 0.0, 0.0])
        && same(green, &[0.0, 1.0, 0.0, 0.0])
        && same(blue, &[0.0, 0.0, 1.0, 0.0]);
    mixer_presets()
        .into_iter()
        .find(|(_, row)| match row {
            None => !monochrome && identity,
            Some(w) => monochrome && same(gray, w),
        })
        .map_or("Custom", |(n, _)| n)
}

/// Photoshop's photo filter colours, 8-bit sRGB.
pub(crate) const PHOTO_FILTERS: [(&str, [u8; 3]); 20] = [
    ("Warming Filter (85)", [236, 138, 0]),
    ("Warming Filter (LBA)", [250, 150, 0]),
    ("Warming Filter (81)", [235, 177, 19]),
    ("Cooling Filter (80)", [0, 109, 255]),
    ("Cooling Filter (LBB)", [0, 93, 255]),
    ("Cooling Filter (82)", [0, 181, 255]),
    ("Red", [234, 26, 26]),
    ("Orange", [243, 132, 23]),
    ("Yellow", [249, 227, 28]),
    ("Green", [25, 201, 25]),
    ("Cyan", [29, 203, 234]),
    ("Blue", [29, 53, 234]),
    ("Violet", [155, 29, 234]),
    ("Magenta", [227, 24, 227]),
    ("Sepia", [172, 122, 51]),
    ("Deep Red", [255, 0, 0]),
    ("Deep Blue", [0, 34, 205]),
    ("Deep Emerald", [0, 140, 0]),
    ("Deep Yellow", [255, 213, 0]),
    ("Underwater", [0, 194, 177]),
];

/// Properties for the newer adjustments. Returns true when an edit
/// finished (the caller then closes the undo step).
pub(crate) fn adjustment_ui(ui: &mut egui::Ui, layer: LayerId, adj: &mut Adjustment) -> bool {
    let mut finished = false;
    match adj {
        Adjustment::GradientMap { gradient, reverse } => {
            let e = gradient_editor(ui, layer, gradient, false);
            finished |= e.finished;
            if check(ui, reverse, "Reverse").changed() {
                finished = true;
            }
            ui.label(
                RichText::new("Shadows take the left of the ramp, highlights the right.")
                    .small()
                    .color(MUTED),
            );
        }
        Adjustment::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
            gray,
        } => {
            let current = mixer_preset_name(red, green, blue, *monochrome, gray);
            combo_row(ui, "Preset", ("mixer-preset", layer), current, |ui| {
                for (name, row) in mixer_presets() {
                    if ui.selectable_label(name == current, name).clicked() {
                        match row {
                            None => {
                                *red = [1.0, 0.0, 0.0, 0.0];
                                *green = [0.0, 1.0, 0.0, 0.0];
                                *blue = [0.0, 0.0, 1.0, 0.0];
                                *monochrome = false;
                            }
                            Some(w) => {
                                *gray = w;
                                *monochrome = true;
                            }
                        }
                        finished = true;
                    }
                }
            });
            let key = egui::Id::new(("mixer-out", layer));
            let mut out: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
            if !*monochrome {
                ui.horizontal(|ui| {
                    row_label(ui, "Output", LABEL_W);
                    segmented(ui, &mut out, &[(0, "Red"), (1, "Green"), (2, "Blue")]);
                });
                ui.data_mut(|d| d.insert_temp(key, out));
            } else {
                ui.horizontal(|ui| {
                    row_label(ui, "Output", LABEL_W);
                    ui.label(RichText::new("Gray").color(TEXT));
                });
            }
            let row = if *monochrome {
                gray
            } else {
                match out {
                    0 => red,
                    1 => green,
                    _ => blue,
                }
            };
            finished |= slider_row_scaled(ui, "Red", &mut row[0], -2.0..=2.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Green", &mut row[1], -2.0..=2.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Blue", &mut row[2], -2.0..=2.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Constant", &mut row[3], -2.0..=2.0, 100.0, "%");
            let total = ((row[0] + row[1] + row[2]) * 100.0).round();
            ui.horizontal(|ui| {
                row_label(ui, "Total", LABEL_W);
                let c = if total > 100.0 { ACCENT } else { MUTED };
                ui.label(RichText::new(format!("{total:+.0}%")).monospace().color(c))
                    .on_hover_text("Above +100% the channel can clip");
            });
            if check(ui, monochrome, "Monochrome").changed() {
                finished = true;
            }
        }
        Adjustment::PhotoFilter {
            color,
            density,
            preserve_luminosity,
        } => {
            let srgb8 = color.map(|c| (linear_to_srgb_f(c) * 255.0).round() as i32);
            let current = PHOTO_FILTERS
                .iter()
                .find(|(_, c)| c.iter().zip(srgb8).all(|(a, b)| (*a as i32 - b).abs() <= 1))
                .map_or("Custom", |(n, _)| n);
            combo_row(ui, "Filter", ("photo-filter", layer), current, |ui| {
                for (name, c) in PHOTO_FILTERS {
                    if ui.selectable_label(name == current, name).clicked() {
                        *color = c.map(|v| srgb_to_linear_f(v as f32 / 255.0));
                        finished = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                row_label(ui, "Color", LABEL_W);
                let mut srgb = color.map(linear_to_srgb_f);
                let r = crate::color_picker::color_edit_button_rgb(ui, &mut srgb);
                if r.changed() {
                    *color = srgb.map(srgb_to_linear_f);
                }
                finished |= r.drag_stopped() || (r.changed() && !r.dragged());
            });
            finished |= slider_row_scaled(ui, "Density", density, 0.0..=1.0, 100.0, "%");
            if check(ui, preserve_luminosity, "Preserve luminosity").changed() {
                finished = true;
            }
        }
        Adjustment::SelectiveColor { colors, absolute } => {
            let key = egui::Id::new(("selc-family", layer));
            let mut fam: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
            combo_row(
                ui,
                "Colors",
                ("selc-colors", layer),
                SELECTIVE_FAMILIES[fam],
                |ui| {
                    for (i, name) in SELECTIVE_FAMILIES.iter().enumerate() {
                        let edited = colors[i].iter().any(|v| *v != 0.0);
                        let label = if edited {
                            format!("{name}  •")
                        } else {
                            (*name).to_string()
                        };
                        ui.selectable_value(&mut fam, i, label);
                    }
                },
            );
            ui.data_mut(|d| d.insert_temp(key, fam));
            let c = &mut colors[fam];
            finished |= slider_row_scaled(ui, "Cyan", &mut c[0], -1.0..=1.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Magenta", &mut c[1], -1.0..=1.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Yellow", &mut c[2], -1.0..=1.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Black", &mut c[3], -1.0..=1.0, 100.0, "%");
            ui.horizontal(|ui| {
                row_label(ui, "Method", LABEL_W);
                let mut abs = *absolute;
                if segmented(ui, &mut abs, &[(false, "Relative"), (true, "Absolute")]) {
                    *absolute = abs;
                    finished = true;
                }
            });
        }
        _ => {}
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the stop editor with real pointer events. The editor sits at
    /// the top-left of a 220 pt wide column, so its strip spans x 7..213
    /// (y 0..24) and the stop markers sit in the band y 26..42.
    struct Rig {
        ctx: egui::Context,
        g: Gradient,
        last: Edit,
    }

    impl Rig {
        fn new() -> Self {
            let mut r = Rig {
                ctx: egui::Context::default(),
                g: Gradient::default(),
                last: Edit::default(),
            };
            r.frame(vec![]);
            r
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 400.0))),
                events,
                ..Default::default()
            };
            let g = &mut self.g;
            let mut out = Edit::default();
            let _ = self.ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        ui.allocate_ui(egui::vec2(220.0, 300.0), |ui| {
                            out = gradient_editor(ui, 7, g, true);
                        });
                    });
            });
            self.last = out;
        }

        fn button(&mut self, p: Pos2, pressed: bool) {
            self.frame(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ]);
        }

        fn click(&mut self, p: Pos2) {
            self.button(p, true);
            self.button(p, false);
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.button(from, true);
            for i in 1..=4 {
                let t = i as f32 / 4.0;
                self.frame(vec![egui::Event::PointerMoved(from + (to - from) * t)]);
            }
            self.button(to, false);
        }
    }

    #[test]
    fn the_channel_mixer_preset_menu_names_the_current_preset() {
        let id = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let gray = [0.4, 0.4, 0.2, 0.0];
        assert_eq!(
            mixer_preset_name(&id[0], &id[1], &id[2], false, &gray),
            "Default (no change)"
        );
        let red = [1.0, 0.0, 0.0, 0.0];
        assert_eq!(
            mixer_preset_name(&id[0], &id[1], &id[2], true, &red),
            "B&W with red filter"
        );
        // Photoshop's own 40/40/20 monochrome mix is no preset here.
        assert_eq!(mixer_preset_name(&id[0], &id[1], &id[2], true, &gray), "Custom");
        let warm = [1.2, 0.0, 0.0, 0.0];
        assert_eq!(mixer_preset_name(&warm, &id[1], &id[2], false, &gray), "Custom");
    }

    #[test]
    fn the_stop_editor_adds_moves_and_removes_stops() {
        let mut rig = Rig::new();
        assert_eq!(rig.g.stops.len(), 2);

        // A click on the strip's middle adds a stop there, coloured like
        // the ramp at that point (sRGB mid grey), and closes the step.
        rig.click(egui::pos2(110.0, 12.0));
        assert_eq!(rig.g.stops.len(), 3);
        let s = rig.g.stops[2];
        assert!((s.pos - 0.5).abs() < 1e-3, "{s:?}");
        assert!((linear_to_srgb_f(s.color[0]) - 0.5).abs() < 2e-3, "{s:?}");
        assert!(rig.last.finished);

        // Dragging the start stop to x = 58.5 moves it to 25%.
        rig.drag(egui::pos2(7.0, 34.0), egui::pos2(58.5, 34.0));
        let first = rig.g.stops[0];
        assert!((first.pos - 0.25).abs() < 1e-3, "{first:?}");
        assert!(rig.last.finished, "the drag's release closes the step");

        // Dragging the middle stop far below the strip removes it...
        rig.drag(egui::pos2(110.0, 34.0), egui::pos2(110.0, 140.0));
        assert_eq!(rig.g.stops.len(), 2);
        // ...but never below two stops.
        rig.drag(egui::pos2(58.5, 34.0), egui::pos2(58.5, 140.0));
        assert_eq!(rig.g.stops.len(), 2);
    }
}
