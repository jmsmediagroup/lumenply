//! Filter > Liquify: a modal workspace, as in Photoshop. Brushes paint a
//! displacement field over a scaled preview of the active pixel layer; OK
//! bakes the field at full resolution with one undoable `LiquifyLayer`.

use super::*;
use lumenply_core::liquify::LiquifyLayer;
use lumenply_render::{liquify_preview, Displacement, LiquifyTool};
use lumenply_tiles::{Rgba, TileStore};
use rayon::prelude::*;

/// Longest preview side in pixels: big enough to judge, small enough to
/// re-warp every frame.
const PREVIEW_MAX: u32 = 1400;
/// Field resolution cap; larger canvases get a coarser node step.
const MAX_NODES: u64 = 4_000_000;
/// Memory kept for stroke undo inside the workspace.
const UNDO_BYTES: usize = 256 << 20;

/// The brushes in the tool column; Alt reverses Twirl.
const TOOLS: [(LiquifyTool, &str, Key); 6] = [
    (LiquifyTool::Push, "Forward warp (W)", Key::W),
    (LiquifyTool::Reconstruct, "Reconstruct (R)", Key::R),
    (LiquifyTool::Smooth, "Smooth (E)", Key::E),
    (LiquifyTool::TwirlCw, "Twirl clockwise (C, Alt reverses)", Key::C),
    (LiquifyTool::Pucker, "Pucker (S)", Key::S),
    (LiquifyTool::Bloat, "Bloat (B)", Key::B),
];

pub(crate) struct LiquifyState {
    layer: LayerId,
    layer_name: String,
    pub(crate) tool: LiquifyTool,
    /// Brush diameter in canvas pixels.
    pub(crate) size: f32,
    pub(crate) pressure: f32,
    pub(crate) rate: f32,
    pub(crate) show_mesh: bool,
    pub(crate) field: Displacement,
    undo: Vec<Displacement>,
    /// The layer over the canvas, scaled by `scale` (premultiplied linear).
    src: Raster,
    scale: f32,
    canvas: Rect,
    tex: Option<egui::TextureHandle>,
    dirty: bool,
    /// Canvas position of the previous dab while a stroke is down.
    last: Option<(f32, f32)>,
}

impl LiquifyState {
    /// Records the field before a stroke, dropping the oldest records
    /// beyond the memory budget.
    fn checkpoint(&mut self) {
        self.undo.push(self.field.clone());
        let each = self.field.d.len() * std::mem::size_of::<[f32; 2]>();
        while self.undo.len() > 1 && self.undo.len() * each > UNDO_BYTES {
            self.undo.remove(0);
        }
    }

    /// One dab of the current tool at canvas point `c`; Push uses the
    /// movement since the last dab, split so no step exceeds a quarter of
    /// the radius (fast drags stay smooth).
    pub(crate) fn dab(&mut self, c: (f32, f32), dt: f32, alt: bool) {
        let radius = self.size / 2.0;
        let tool = match self.tool {
            LiquifyTool::TwirlCw if alt => LiquifyTool::TwirlCcw,
            t => t,
        };
        if tool == LiquifyTool::Push {
            let Some(last) = self.last else { return };
            let (dx, dy) = (c.0 - last.0, c.1 - last.1);
            let len = (dx * dx + dy * dy).sqrt();
            let n = (len / (radius / 4.0).max(1.0)).ceil().max(1.0) as usize;
            for k in 1..=n {
                let t = k as f32 / n as f32;
                let at = (last.0 + dx * t, last.1 + dy * t);
                let step = (dx / n as f32, dy / n as f32);
                self.field.dab(tool, at, radius, self.pressure, 1.0, step);
            }
        } else {
            // Stationary brushes act over time while held, like Photoshop's
            // rate: one second at 100% is three full-strength dabs.
            let rate = (self.rate * dt * 3.0).min(1.0);
            self.field.dab(tool, c, radius, self.pressure, rate, (0.0, 0.0));
        }
        self.dirty = true;
    }
}

/// The canvas area of `store`, box-filtered down by `scale` (≤ 1).
pub(crate) fn downscale(store: &TileStore, rect: Rect, scale: f32) -> Raster {
    if scale >= 1.0 {
        return store.to_raster(rect);
    }
    let w = ((rect.w as f32 * scale).round() as u32).max(1);
    let h = ((rect.h as f32 * scale).round() as u32).max(1);
    let mut out = Raster::new(w, h);
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let sy0 = (y as f32 / scale).floor() as u32;
            let sy1 = (((y + 1) as f32 / scale).floor() as u32).clamp(sy0 + 1, rect.h);
            let band = store.to_raster(Rect::new(rect.x, rect.y + sy0 as i32, rect.w, sy1 - sy0));
            for (x, p) in row.iter_mut().enumerate() {
                let sx0 = (x as f32 / scale).floor() as u32;
                let sx1 = (((x + 1) as f32 / scale).floor() as u32).clamp(sx0 + 1, rect.w);
                let mut acc = [0.0f32; 4];
                for by in 0..band.height {
                    for bx in sx0..sx1 {
                        let q = band.get(bx, by);
                        acc[0] += q.r;
                        acc[1] += q.g;
                        acc[2] += q.b;
                        acc[3] += q.a;
                    }
                }
                let n = (band.height * (sx1 - sx0)) as f32;
                *p = Rgba::new(acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n);
            }
        });
    out
}

/// Premultiplied linear pixels over a light checkerboard, sRGB-encoded.
pub(crate) fn to_image(r: &Raster) -> egui::ColorImage {
    let (light, dark) = (
        lumenply_io::srgb_to_linear_f(0.80),
        lumenply_io::srgb_to_linear_f(0.66),
    );
    let w = r.width as usize;
    let mut px = vec![Color32::BLACK; r.pixels.len()];
    px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let p = r.pixels[y * w + x];
            let bg = if (x / 8 + y / 8) % 2 == 0 { light } else { dark };
            let k = 1.0 - p.a.clamp(0.0, 1.0);
            *out = Color32::from_rgb(
                lumenply_io::linear_to_srgb(p.r + bg * k),
                lumenply_io::linear_to_srgb(p.g + bg * k),
                lumenply_io::linear_to_srgb(p.b + bg * k),
            );
        }
    });
    egui::ColorImage {
        size: [w, r.height as usize],
        pixels: px,
    }
}

/// A small line glyph for each brush.
fn draw_tool_icon(p: &egui::Painter, r: egui::Rect, tool: LiquifyTool, ink: Color32) {
    let s = Stroke::new(1.5, ink);
    let c = r.center();
    let u = r.width() / 2.0;
    let arrow = |from: Pos2, to: Pos2| {
        p.line_segment([from, to], s);
        let d = (to - from).normalized() * (u * 0.35);
        let n = egui::vec2(-d.y, d.x);
        p.line_segment([to, to - d + n * 0.8], s);
        p.line_segment([to, to - d - n * 0.8], s);
    };
    match tool {
        LiquifyTool::Push => {
            p.circle_stroke(c + egui::vec2(-u * 0.35, 0.0), u * 0.45, s);
            arrow(c + egui::vec2(-u * 0.1, 0.0), c + egui::vec2(u * 0.9, 0.0));
        }
        LiquifyTool::Reconstruct => {
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = std::f32::consts::PI * (0.25 + 1.5 * i as f32 / 20.0);
                    c + egui::vec2(a.cos(), a.sin()) * (u * 0.7)
                })
                .collect();
            p.add(egui::Shape::line(pts.clone(), s));
            let end = pts[pts.len() - 1];
            p.line_segment([end, end + egui::vec2(u * 0.4, 0.0)], s);
            p.line_segment([end, end + egui::vec2(0.0, -u * 0.4)], s);
        }
        LiquifyTool::Smooth => {
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    c + egui::vec2((t - 0.5) * 1.8 * u, (t * std::f32::consts::TAU).sin() * u * 0.35)
                })
                .collect();
            p.add(egui::Shape::line(pts, s));
        }
        LiquifyTool::TwirlCw | LiquifyTool::TwirlCcw => {
            let pts: Vec<Pos2> = (0..=40)
                .map(|i| {
                    let t = i as f32 / 40.0;
                    let a = t * std::f32::consts::TAU * 1.6;
                    c + egui::vec2(a.cos(), a.sin()) * (u * 0.85 * t)
                })
                .collect();
            p.add(egui::Shape::line(pts, s));
        }
        LiquifyTool::Pucker | LiquifyTool::Bloat => {
            let inward = tool == LiquifyTool::Pucker;
            for d in [
                egui::vec2(1.0, 0.0),
                egui::vec2(-1.0, 0.0),
                egui::vec2(0.0, 1.0),
                egui::vec2(0.0, -1.0),
            ] {
                let (a, b) = (c + d * (u * 0.95), c + d * (u * 0.3));
                if inward {
                    arrow(a, b);
                } else {
                    arrow(b, a);
                }
            }
        }
    }
}

impl App {
    /// Opens the workspace on the active pixel layer (the action registry
    /// blocks other layer kinds with a reason).
    pub(crate) fn open_liquify(&mut self) {
        let Some(id) = self.active else { return };
        let doc = self.editor.doc();
        let Some(layer) = doc.layer(id) else { return };
        let Some(store) = layer.pixels() else { return };
        let canvas = doc.canvas();
        let scale = (PREVIEW_MAX as f32 / canvas.w.max(canvas.h) as f32).min(1.0);
        let src = downscale(store, canvas, scale);
        let field = Displacement::new(canvas, Displacement::step_for(canvas, MAX_NODES));
        self.liquify = Some(Box::new(LiquifyState {
            layer: id,
            layer_name: layer.name.clone(),
            tool: LiquifyTool::Push,
            size: (canvas.w.min(canvas.h) as f32 / 6.0).clamp(10.0, 600.0).round(),
            pressure: 0.5,
            rate: 0.5,
            show_mesh: false,
            field,
            undo: Vec::new(),
            src,
            scale,
            canvas,
            tex: None,
            dirty: true,
            last: None,
        }));
    }

    /// The workspace; replaces the editor UI while open.
    pub(crate) fn liquify_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.liquify.take() else { return };
        let mut done: Option<bool> = None;
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(Key::Escape) {
                    done = Some(false);
                }
                if i.key_pressed(Key::Enter) {
                    done = Some(true);
                }
                if i.modifiers.command && i.key_pressed(Key::Z) {
                    if let Some(prev) = st.undo.pop() {
                        st.field = prev;
                        st.dirty = true;
                    }
                } else if !i.modifiers.command {
                    for (tool, _, key) in TOOLS {
                        if i.key_pressed(key) {
                            st.tool = tool;
                        }
                    }
                }
                if i.key_pressed(Key::OpenBracket) {
                    st.size = (st.size / 1.15).max(2.0).round();
                }
                if i.key_pressed(Key::CloseBracket) {
                    st.size = (st.size * 1.15).min(1500.0).round();
                }
            });
        }

        egui::TopBottomPanel::top("liquify-bar")
            .frame(bar_frame())
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Liquify").strong().color(TEXT));
                    ui.label(RichText::new(&st.layer_name).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Drag to warp  ·  [ ] brush size  ·  Cmd+Z undo stroke")
                                .color(MUTED),
                        );
                    });
                });
            });

        egui::SidePanel::left("liquify-tools")
            .resizable(false)
            .exact_width(52.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(8.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (tool, tip, _) in TOOLS {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), Sense::click());
                    let on = st.tool == tool;
                    let fill = if on {
                        ACCENT
                    } else if resp.hovered() {
                        HOVER
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, RADIUS, fill);
                    let ink = if on { ACCENT_INK } else { TEXT };
                    draw_tool_icon(ui.painter(), rect.shrink(9.0), tool, ink);
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tool.name())
                    });
                    if resp.on_hover_text(tip).clicked() {
                        st.tool = tool;
                    }
                }
            });

        egui::SidePanel::right("liquify-props")
            .resizable(false)
            .exact_width(268.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                ui.label(RichText::new("BRUSH").small().strong().color(MUTED));
                ui.add_space(4.0);
                let opts = RowOpts {
                    label_w: 70.0,
                    log: true,
                    ..RowOpts::default()
                };
                slider_row_ex(ui, "Size", &mut st.size, 2.0..=1500.0, " px", opts);
                slider_row_scaled_w(ui, "Pressure", &mut st.pressure, 0.01..=1.0, 100.0, "%", 70.0);
                ui.add_enabled_ui(st.tool != LiquifyTool::Push, |ui| {
                    slider_row_scaled_w(ui, "Rate", &mut st.rate, 0.01..=1.0, 100.0, "%", 70.0);
                });
                ui.add_space(10.0);
                ui.label(RichText::new("VIEW").small().strong().color(MUTED));
                ui.add_space(4.0);
                check(ui, &mut st.show_mesh, "Show mesh");
                ui.add_space(10.0);
                ui.label(RichText::new("RECONSTRUCT").small().strong().color(MUTED));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let any = !st.field.is_identity();
                    if ui
                        .add_enabled(any, egui::Button::new("Restore all"))
                        .on_hover_text("Undo every warp (Cmd+Z brings it back)")
                        .clicked()
                    {
                        st.checkpoint();
                        st.field = Displacement::new(st.canvas, st.field.step);
                        st.dirty = true;
                    }
                    if ui
                        .add_enabled(!st.undo.is_empty(), egui::Button::new("Undo stroke"))
                        .clicked()
                    {
                        if let Some(prev) = st.undo.pop() {
                            st.field = prev;
                            st.dirty = true;
                        }
                    }
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(primary_button("OK")).clicked() {
                            done = Some(true);
                        }
                        if ui.add(footer_button("Cancel")).clicked() {
                            done = Some(false);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(avail, Sense::click_and_drag());
                a11y_name(&resp, "Liquify preview");
                let (cw, ch) = (st.canvas.w as f32, st.canvas.h as f32);
                let disp = ((avail.width() - 48.0) / cw)
                    .min((avail.height() - 48.0) / ch)
                    .max(0.01);
                let img = egui::Rect::from_center_size(avail.center(), egui::vec2(cw * disp, ch * disp));
                if st.dirty || st.tex.is_none() {
                    let warped = liquify_preview(
                        &st.src,
                        (st.canvas.x as f32, st.canvas.y as f32),
                        st.scale,
                        &st.field,
                    );
                    let image = to_image(&warped);
                    match &mut st.tex {
                        Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            st.tex = Some(ctx.load_texture("liquify", image, egui::TextureOptions::LINEAR))
                        }
                    }
                    st.dirty = false;
                }
                let p = ui.painter_at(avail);
                if let Some(t) = &st.tex {
                    p.image(
                        t.id(),
                        img,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                p.rect_stroke(img, 0.0, Stroke::new(1.0, LINE));
                let to_screen = |x: f32, y: f32| {
                    egui::pos2(
                        img.min.x + (x - st.canvas.x as f32) * disp,
                        img.min.y + (y - st.canvas.y as f32) * disp,
                    )
                };
                if st.show_mesh {
                    let n = 24.0;
                    let g = cw.max(ch) / n;
                    let ink = Stroke::new(1.0, Color32::from_rgba_unmultiplied(225, 228, 232, 120));
                    let line = |pts: Vec<Pos2>| egui::Shape::line(pts, ink);
                    let mut y = st.canvas.y as f32;
                    while y <= st.canvas.bottom() as f32 {
                        let pts = (0..=(cw / (g / 4.0)) as usize)
                            .map(|i| {
                                let x = st.canvas.x as f32 + i as f32 * g / 4.0;
                                let d = st.field.at(x, y);
                                to_screen(x - d[0], y - d[1])
                            })
                            .collect();
                        p.add(line(pts));
                        y += g;
                    }
                    let mut x = st.canvas.x as f32;
                    while x <= st.canvas.right() as f32 {
                        let pts = (0..=(ch / (g / 4.0)) as usize)
                            .map(|i| {
                                let y = st.canvas.y as f32 + i as f32 * g / 4.0;
                                let d = st.field.at(x, y);
                                to_screen(x - d[0], y - d[1])
                            })
                            .collect();
                        p.add(line(pts));
                        x += g;
                    }
                }
                let down = resp.is_pointer_button_down_on();
                if let Some(pos) = resp.hover_pos().or(resp.interact_pointer_pos()) {
                    let c = (
                        st.canvas.x as f32 + (pos.x - img.min.x) / disp,
                        st.canvas.y as f32 + (pos.y - img.min.y) / disp,
                    );
                    let r = st.size / 2.0 * disp;
                    p.circle_stroke(pos, r, Stroke::new(2.5, Color32::from_black_alpha(140)));
                    p.circle_stroke(pos, r, Stroke::new(1.0, Color32::WHITE));
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                    if down {
                        if st.last.is_none() {
                            st.checkpoint();
                            st.last = Some(c);
                        }
                        let (dt, alt) = ctx.input(|i| (i.stable_dt.min(0.1), i.modifiers.alt));
                        st.dab(c, dt, alt);
                        st.last = Some(c);
                        ctx.request_repaint();
                    }
                }
                if !down {
                    st.last = None;
                }
            });

        match done {
            Some(true) => {
                if !st.field.is_identity() {
                    self.run(&LiquifyLayer {
                        layer: st.layer,
                        field: std::sync::Arc::new(st.field),
                    });
                }
                self.liquify = None;
            }
            Some(false) => {
                self.status = "Liquify cancelled".into();
                self.liquify = None;
            }
            None => self.liquify = Some(st),
        }
    }

    /// `liquify:demo` paints a few strokes (a push, a bloat, a twirl) so the
    /// workspace can be captured with a visible warp; `liquify:mesh` shows
    /// the mesh; `liquify:ok` commits.
    pub(crate) fn debug_liquify(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("liquify:") else {
            return false;
        };
        if self.liquify.is_none() {
            self.open_liquify();
        }
        match rest {
            "open" => {}
            "demo" => {
                // Visible on the demo photo: the main peak pushed up, the
                // lake bloated, a cloud twirled.
                if let Some(st) = self.liquify.as_mut() {
                    let (w, h) = (st.canvas.w as f32, st.canvas.h as f32);
                    st.checkpoint();
                    st.pressure = 1.0;
                    st.size = w * 0.12;
                    st.tool = LiquifyTool::Push;
                    let (x0, y0) = (w * 0.205, h * 0.47);
                    st.last = Some((x0, y0));
                    for i in 1..=24 {
                        let c = (x0, y0 - i as f32 * h * 0.004);
                        st.dab(c, 1.0 / 60.0, false);
                        st.last = Some(c);
                    }
                    st.last = None;
                    st.size = w * 0.14;
                    st.tool = LiquifyTool::Bloat;
                    for _ in 0..40 {
                        st.dab((w * 0.37, h * 0.83), 1.0 / 30.0, false);
                    }
                    st.size = w * 0.2;
                    st.tool = LiquifyTool::TwirlCw;
                    for _ in 0..40 {
                        st.dab((w * 0.7, h * 0.28), 1.0 / 30.0, false);
                    }
                    st.tool = LiquifyTool::Push;
                }
            }
            "mesh" => {
                if let Some(st) = self.liquify.as_mut() {
                    st.show_mesh = true;
                }
            }
            "ok" => {
                if let Some(st) = self.liquify.take() {
                    self.run(&LiquifyLayer {
                        layer: st.layer,
                        field: std::sync::Arc::new(st.field),
                    });
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

    fn launch_tiny() -> App {
        let mut doc = Document::new(96, 64);
        let id = doc.add_pixel_layer("Background");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(30, 20, Rgba::new(1.0, 0.0, 0.0, 1.0));
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app
    }

    #[test]
    fn the_workspace_warps_the_layer_with_one_undo_step() {
        let mut app = launch_tiny();
        let id = app.active.unwrap();
        let steps = app.editor.history().len();
        app.run_menu_action("liquify");
        let st = app.liquify.as_mut().expect("open");
        assert_eq!(st.scale, 1.0, "small canvases preview at full size");
        st.tool = LiquifyTool::Push;
        st.size = 40.0;
        st.pressure = 1.0;
        st.last = Some((30.0, 20.0));
        st.dab((34.0, 20.0), 1.0 / 60.0, false);
        // The field at the dab centre moves content 4 px to the right.
        let d = st.field.at(34.0, 20.0);
        assert!((d[0] + 4.0).abs() < 0.3, "{d:?}");
        assert!(app.debug_liquify(&egui::Context::default(), "liquify:ok"));
        assert!(app.liquify.is_none());
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last(), Some(&"Liquify"));
        let red = app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(34, 20);
        assert!(red.r > 0.5 && red.g < 0.2, "the red dot moved right: {red:?}");
    }

    #[test]
    fn liquify_is_blocked_on_layers_it_cannot_warp() {
        let mut app = launch_tiny();
        app.add_adjustment(lumenply_doc::Adjustment::Invert);
        assert!(app.action_block("liquify").is_some());
        app.run_menu_action("liquify");
        assert!(app.liquify.is_none());
    }

    #[test]
    fn stroke_undo_and_cancel_leave_the_document_alone() {
        let mut app = launch_tiny();
        let steps = app.editor.history().len();
        app.run_menu_action("liquify");
        let st = app.liquify.as_mut().unwrap();
        st.checkpoint();
        st.tool = LiquifyTool::Bloat;
        st.dab((48.0, 32.0), 0.1, false);
        assert!(!st.field.is_identity());
        let prev = st.undo.pop().unwrap();
        st.field = prev;
        assert!(st.field.is_identity(), "undo stroke restores the field");
        app.liquify = None; // Cancel
        assert_eq!(app.editor.history().len(), steps);
    }

    #[test]
    fn every_control_in_the_workspace_has_a_spoken_name() {
        let mut app = launch_tiny();
        app.run_menu_action("liquify");
        let ctx = crate::a11y_tests::ctx();
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.liquify.is_some(), "still open");
        assert_eq!(missing, Vec::<String>::new());
    }

    #[test]
    fn downscaled_previews_average_their_source() {
        let mut r = Raster::new(4, 2);
        for x in 0..4 {
            for y in 0..2 {
                let v = if x < 2 { 1.0 } else { 0.0 };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let store = TileStore::from_raster(&r, 0, 0);
        let small = downscale(&store, Rect::new(0, 0, 4, 2), 0.5);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.get(0, 0), Rgba::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(small.get(1, 0), Rgba::new(0.0, 0.0, 0.0, 1.0));
    }
}
