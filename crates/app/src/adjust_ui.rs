//! Properties sections for Gradient Map, Channel Mixer, Photo Filter and
//! Selective Color, and the gradient stop editor they share with gradient
//! fill layers. Per-panel UI state (selected stop, output channel, colour
//! family) lives in egui's memory, keyed by layer, so `App` stays as is.

use super::*;
use crate::color_picker::{format_hex, paint_swatch, picker_popup, Placement};
use lumenply_doc::adjust::SELECTIVE_FAMILIES;
use lumenply_doc::{Gradient, GradientStop};
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
            combo_row(ui, "Preset", ("mixer-preset", layer), "Choose…", |ui| {
                for (name, row) in mixer_presets() {
                    if ui.selectable_label(false, name).clicked() {
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
