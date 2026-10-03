//! Edit ▸ Content-Aware Scale (Alt+Shift+Cmd+C): a full-window workspace,
//! as Puppet Warp is. The active pixel layer's painted bounds get eight
//! handles over the composite below it; dragging a handle (or typing the
//! W/H percentages) rescales the layer live at preview resolution with
//! seam carving, a drag inside the box moves it. The side panel holds
//! Amount, Protect (none, the selection or a saved channel) and Protect
//! skin tones, as Photoshop's options bar does. Apply (Enter) runs one
//! `ContentAwareScale` at full size: one undo step. Esc cancels.

use super::*;
use lumenply_core::content_aware_scale::ContentAwareScale;
use lumenply_render::seam_carve::{content_aware_scale, CarveOptions};
use lumenply_tiles::TileStore;
use rayon::prelude::*;

/// Longest preview side in pixels. Carving cost grows with the area, so
/// this is smaller than the other workspaces' previews.
const PREVIEW_MAX: u32 = 720;
/// Screen-pixel reach of the handles.
const HANDLE_REACH: f32 = 9.0;
/// While dragging, re-carve every frame only if a carve takes less than
/// this; otherwise the last result stretches until the drag ends.
const LIVE_MS: f32 = 60.0;

/// What the seams must avoid.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Protect {
    None,
    Selection,
    /// A saved selection (alpha channel), by index.
    Channel(usize),
}

/// Which edges a press grabbed (none of them: the whole box moves).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Grip {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Grip {
    fn is_move(&self) -> bool {
        !(self.left || self.right || self.top || self.bottom)
    }

    fn is_corner(&self) -> bool {
        (self.left || self.right) && (self.top || self.bottom)
    }
}

pub(crate) struct CasState {
    layer: LayerId,
    layer_name: String,
    /// The layer's painted bounds: what gets scaled.
    src_rect: Rect,
    /// The target box in canvas pixels: x0, y0, x1, y1.
    pub(crate) bx: [f32; 4],
    /// Amount, 0..=100 %.
    pub(crate) amount: f32,
    pub(crate) protect: Protect,
    pub(crate) skin: bool,
    /// The layer's bounds at preview scale.
    src: Raster,
    /// The composite under the layer, the whole canvas at preview scale.
    backdrop: Raster,
    /// Protection at preview scale, for the protect choice it was made for.
    protect_cache: Option<(Protect, Option<Vec<f32>>)>,
    opacity: f32,
    scale: f32,
    canvas: Rect,
    back_tex: Option<egui::TextureHandle>,
    layer_tex: Option<egui::TextureHandle>,
    /// The preview carve's inputs, to skip carving when nothing changed.
    key: Option<(u32, u32, u32, Protect, bool)>,
    /// A carve is due (inputs changed while it was deferred).
    stale: bool,
    drag: Option<(Grip, [f32; 4], (f32, f32))>,
    hover: Option<Grip>,
    /// Where the box was drawn last frame, in screen points.
    screen_box: egui::Rect,
    pub(crate) carve_ms: f32,
}

/// The composite of what lies under `layer`: everything below its
/// top-level ancestor, plus that group's other contents.
fn composite_below(doc: &Document, layer: LayerId, rect: Rect) -> TileStore {
    let mut top = layer;
    while let Some(p) = doc.parent_of(top) {
        top = p;
    }
    let Some(i) = doc.layers().iter().position(|l| l.id == top) else {
        return TileStore::new();
    };
    let mut d = doc.clone();
    if let Some(l) = d.layer_mut(layer) {
        l.visible = false;
    }
    lumenply_render::composite_layers(&d.layers()[..=i], rect, d.canvas())
}

/// Premultiplied linear pixels as an egui texture with alpha.
fn layer_image(r: &Raster) -> egui::ColorImage {
    let px: Vec<Color32> = r
        .pixels
        .par_iter()
        .map(|p| {
            if p.a <= 0.0 {
                return Color32::TRANSPARENT;
            }
            let k = 1.0 / p.a;
            Color32::from_rgba_unmultiplied(
                lumenply_io::linear_to_srgb(p.r * k),
                lumenply_io::linear_to_srgb(p.g * k),
                lumenply_io::linear_to_srgb(p.b * k),
                (p.a.clamp(0.0, 1.0) * 255.0).round() as u8,
            )
        })
        .collect();
    egui::ColorImage {
        size: [r.width as usize, r.height as usize],
        pixels: px,
    }
}

impl CasState {
    /// The box rounded to whole pixels: origin and size.
    pub(crate) fn target(&self) -> (i32, i32, u32, u32) {
        let [x0, y0, x1, y1] = self.bx;
        let (ox, oy) = (x0.round() as i32, y0.round() as i32);
        let w = ((x1.round() as i32) - ox).max(1) as u32;
        let h = ((y1.round() as i32) - oy).max(1) as u32;
        (ox, oy, w, h)
    }

    /// Width and height as percentages of the original bounds.
    pub(crate) fn percent(&self) -> (f32, f32) {
        let [x0, y0, x1, y1] = self.bx;
        (
            (x1 - x0) / self.src_rect.w as f32 * 100.0,
            (y1 - y0) / self.src_rect.h as f32 * 100.0,
        )
    }

    /// Resize the box to these percentages about its centre (Photoshop's
    /// default reference point).
    pub(crate) fn set_percent(&mut self, wp: f32, hp: f32) {
        let [x0, y0, x1, y1] = self.bx;
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let w = (self.src_rect.w as f32 * wp / 100.0).max(1.0);
        let h = (self.src_rect.h as f32 * hp / 100.0).max(1.0);
        self.bx = [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0];
    }

    /// What the view fits: the canvas and the box (the box a drag
    /// started from, while dragging).
    fn view_bounds(&self) -> [f32; 4] {
        let b = self.drag.map_or(self.bx, |d| d.1);
        let c = self.canvas;
        [
            b[0].min(c.x as f32),
            b[1].min(c.y as f32),
            b[2].max(c.right() as f32),
            b[3].max(c.bottom() as f32),
        ]
    }

    /// The command Apply runs, or `None` when the box is unchanged.
    fn command(&self, doc: &Document) -> Option<ContentAwareScale> {
        let (ox, oy, w, h) = self.target();
        let s = self.src_rect;
        if (ox, oy, w, h) == (s.x, s.y, s.w, s.h) {
            return None;
        }
        Some(ContentAwareScale {
            layer: self.layer,
            new_w: w,
            new_h: h,
            origin: Some((ox, oy)),
            amount: self.amount / 100.0,
            protect: protect_mask(doc, self.protect),
            skin: self.skin,
        })
    }

    /// Protection at preview scale (nearest sample of the full mask).
    fn protect_preview(&mut self, doc: &Document) -> Option<Vec<f32>> {
        if let Some((p, v)) = &self.protect_cache {
            if *p == self.protect {
                return v.clone();
            }
        }
        let v = protect_mask(doc, self.protect).map(|m| {
            let full = m.to_dense(self.src_rect);
            let (sw, pw, ph) = (
                self.src_rect.w as usize,
                self.src.width as usize,
                self.src.height as usize,
            );
            let (kx, ky) = (
                self.src_rect.w as f32 / pw as f32,
                self.src_rect.h as f32 / ph as f32,
            );
            let mut out = vec![0.0f32; pw * ph];
            for y in 0..ph {
                let sy = (((y as f32 + 0.5) * ky) as usize).min(self.src_rect.h as usize - 1);
                for x in 0..pw {
                    let sx = (((x as f32 + 0.5) * kx) as usize).min(sw - 1);
                    out[y * pw + x] = full[sy * sw + sx];
                }
            }
            out
        });
        self.protect_cache = Some((self.protect, v.clone()));
        v
    }

    /// Carves the preview when its inputs changed (or when forced).
    fn carve(&mut self, ctx: &egui::Context, doc: &Document) {
        let [x0, y0, x1, y1] = self.bx;
        let pw = (((x1 - x0) * self.scale).round() as u32).max(1);
        let ph = (((y1 - y0) * self.scale).round() as u32).max(1);
        let key = (pw, ph, self.amount.to_bits(), self.protect, self.skin);
        if self.key == Some(key) && self.layer_tex.is_some() {
            self.stale = false;
            return;
        }
        let t = std::time::Instant::now();
        let protect = self.protect_preview(doc);
        let opts = CarveOptions {
            amount: self.amount / 100.0,
            protect: protect.as_deref(),
            skin: self.skin,
        };
        let out = content_aware_scale(&self.src, pw, ph, &opts);
        let img = layer_image(&out);
        match &mut self.layer_tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.layer_tex = Some(ctx.load_texture("cas-layer", img, egui::TextureOptions::LINEAR)),
        }
        self.key = Some(key);
        self.stale = false;
        self.carve_ms = t.elapsed().as_secs_f32() * 1000.0;
    }

    /// The grip under screen point `p` for the box drawn at `r`.
    fn grip_at(r: egui::Rect, p: Pos2) -> Option<Grip> {
        let near = |a: f32, b: f32| (a - b).abs() <= HANDLE_REACH;
        let inside_x = p.x >= r.min.x - HANDLE_REACH && p.x <= r.max.x + HANDLE_REACH;
        let inside_y = p.y >= r.min.y - HANDLE_REACH && p.y <= r.max.y + HANDLE_REACH;
        if !(inside_x && inside_y) {
            return None;
        }
        let g = Grip {
            left: near(p.x, r.min.x),
            right: near(p.x, r.max.x) && !near(p.x, r.min.x),
            top: near(p.y, r.min.y),
            bottom: near(p.y, r.max.y) && !near(p.y, r.min.y),
        };
        if g.is_move() && !r.contains(p) {
            return None;
        }
        Some(g)
    }

    /// The box after dragging `grip` from `press` to `to` (canvas px);
    /// `keep` holds the start box's proportions on corners.
    fn dragged(start: [f32; 4], grip: Grip, press: (f32, f32), to: (f32, f32), keep: bool) -> [f32; 4] {
        let (dx, dy) = (to.0 - press.0, to.1 - press.1);
        let [mut x0, mut y0, mut x1, mut y1] = start;
        if grip.is_move() {
            return [x0 + dx, y0 + dy, x1 + dx, y1 + dy];
        }
        if grip.left {
            x0 = (x0 + dx).min(x1 - 1.0);
        }
        if grip.right {
            x1 = (x1 + dx).max(x0 + 1.0);
        }
        if grip.top {
            y0 = (y0 + dy).min(y1 - 1.0);
        }
        if grip.bottom {
            y1 = (y1 + dy).max(y0 + 1.0);
        }
        if keep && grip.is_corner() {
            let (sw, sh) = (start[2] - start[0], start[3] - start[1]);
            let k = ((x1 - x0) / sw).max((y1 - y0) / sh);
            let (w, h) = (sw * k, sh * k);
            if grip.left {
                x0 = x1 - w;
            } else {
                x1 = x0 + w;
            }
            if grip.top {
                y0 = y1 - h;
            } else {
                y1 = y0 + h;
            }
        }
        [x0, y0, x1, y1]
    }
}

/// The coverage a protect choice stands for.
fn protect_mask(doc: &Document, p: Protect) -> Option<lumenply_doc::Mask> {
    match p {
        Protect::None => None,
        Protect::Selection => doc.selection.as_ref().map(|s| s.coverage.clone()),
        Protect::Channel(i) => doc.saved_selections.get(i).map(|s| s.mask.clone()),
    }
}

impl App {
    /// Why Content-Aware Scale can't open on the active layer, if it can't.
    pub(crate) fn cas_block(&self) -> Option<&'static str> {
        let Some(layer) = self.active_layer() else {
            return Some("Select a pixel layer first");
        };
        if layer.smart_layer().is_some()
            || layer.text_layer().is_some()
            || layer.shape_layer().is_some()
            || layer.fill_layer().is_some()
        {
            return Some("Rasterize the layer first");
        }
        if !self.active_is_pixel() {
            return Some("Select a pixel layer first");
        }
        if layer.pixels().and_then(|s| s.content_bounds()).is_none() {
            return Some("The layer is empty");
        }
        self.lock_block(layer_actions::LockNeed::Reshape)
    }

    /// Opens the workspace on the active pixel layer.
    pub(crate) fn open_cas(&mut self) {
        let Some(id) = self.active else { return };
        let doc = self.editor.doc();
        let Some(layer) = doc.layer(id) else { return };
        let Some(store) = layer.pixels() else { return };
        let Some(src_rect) = store.content_bounds() else {
            self.status = "Content-Aware Scale needs a layer with pixels".into();
            return;
        };
        let canvas = doc.canvas();
        let side = canvas.w.max(canvas.h).max(src_rect.w).max(src_rect.h);
        let scale = (PREVIEW_MAX as f32 / side as f32).min(1.0);
        let src = liquify::downscale(store, src_rect, scale);
        let below = composite_below(doc, id, canvas);
        let backdrop = liquify::downscale(&below, canvas, scale);
        let r = src_rect;
        self.cas = Some(Box::new(CasState {
            layer: id,
            layer_name: layer.name.clone(),
            src_rect,
            bx: [r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32],
            amount: 100.0,
            // Nothing protected until the user picks something, as in
            // Photoshop (the selection is offered in the menu).
            protect: Protect::None,
            skin: false,
            src,
            backdrop,
            protect_cache: None,
            opacity: layer.opacity,
            scale,
            canvas,
            back_tex: None,
            layer_tex: None,
            key: None,
            stale: true,
            drag: None,
            hover: None,
            screen_box: egui::Rect::NOTHING,
            carve_ms: 0.0,
        }));
    }

    /// Commits the scale (when the box changed) and closes the workspace.
    fn finish_cas(&mut self, st: Box<CasState>) {
        let Some(cmd) = st.command(self.editor.doc()) else {
            self.status = "Content-Aware Scale: nothing changed".into();
            return;
        };
        let before = self.editor.history().len();
        let t = std::time::Instant::now();
        self.run(&cmd);
        if self.editor.history().len() > before {
            self.status = format!(
                "Content-Aware Scale to {} × {} px in {:.0} ms",
                cmd.new_w,
                cmd.new_h,
                t.elapsed().as_secs_f32() * 1000.0
            );
        }
    }

    /// The workspace; replaces the editor UI while open.
    pub(crate) fn cas_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.cas.take() else { return };
        let mut done: Option<bool> = None;
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(Key::Escape) {
                    done = Some(false);
                }
                if i.key_pressed(Key::Enter) {
                    done = Some(true);
                }
            });
        }

        egui::TopBottomPanel::top("cas-bar")
            .frame(bar_frame())
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Content-Aware Scale").strong().color(TEXT));
                    ui.label(RichText::new(&st.layer_name).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                "Drag a handle to scale  ·  Shift keeps proportions  ·  drag inside to move",
                            )
                            .color(MUTED),
                        );
                    });
                });
            });

        let saved: Vec<String> = self
            .editor
            .doc()
            .saved_selections
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let has_sel = self.editor.doc().selection.is_some();
        egui::SidePanel::right("cas-props")
            .resizable(false)
            .exact_width(268.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                ui.label(RichText::new("SIZE").small().strong().color(MUTED));
                ui.add_space(4.0);
                let (mut wp, mut hp) = st.percent();
                let field = |ui: &mut egui::Ui, v: &mut f32, name: &str| {
                    ui.label(RichText::new(name).color(MUTED));
                    let r = num_field(
                        ui,
                        egui::DragValue::new(v)
                            .range(1.0..=400.0)
                            .speed(0.5)
                            .max_decimals(1)
                            .suffix(" %"),
                        74.0,
                    );
                    a11y_name(&r, &format!("{name} percent"));
                    r.changed()
                };
                let changed = ui
                    .horizontal(|ui| {
                        let a = field(ui, &mut wp, "W");
                        let b = field(ui, &mut hp, "H");
                        a || b
                    })
                    .inner;
                if changed {
                    st.set_percent(wp, hp);
                }
                let (_, _, w, h) = st.target();
                ui.label(
                    RichText::new(format!(
                        "{} × {} px  (from {} × {})",
                        w, h, st.src_rect.w, st.src_rect.h
                    ))
                    .monospace()
                    .color(MUTED),
                );
                ui.add_space(10.0);
                ui.label(RichText::new("AMOUNT").small().strong().color(MUTED));
                ui.add_space(4.0);
                let opts = RowOpts {
                    label_w: 70.0,
                    int: true,
                    ..RowOpts::default()
                };
                slider_row_ex(ui, "Amount", &mut st.amount, 0.0..=100.0, " %", opts);
                ui.label(
                    RichText::new("100 % carves seams only; 0 % is a plain resize")
                        .small()
                        .color(MUTED),
                );
                ui.add_space(10.0);
                ui.label(RichText::new("PROTECT").small().strong().color(MUTED));
                ui.add_space(4.0);
                let name = |p: Protect| match p {
                    Protect::None => "None".to_string(),
                    Protect::Selection => "Selection".to_string(),
                    Protect::Channel(i) => saved.get(i).cloned().unwrap_or_else(|| "None".into()),
                };
                let r = egui::ComboBox::from_id_salt("cas-protect")
                    .selected_text(name(st.protect))
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        popup_style(ui);
                        let mut opts = vec![Protect::None];
                        if has_sel {
                            opts.push(Protect::Selection);
                        }
                        opts.extend((0..saved.len()).map(Protect::Channel));
                        for p in opts {
                            if ui.selectable_label(st.protect == p, name(p)).clicked() {
                                st.protect = p;
                            }
                        }
                    });
                a11y_name(&r.response, "Protect");
                r.response
                    .on_hover_text("Seams go round this selection or saved channel");
                ui.add_space(4.0);
                check(ui, &mut st.skin, "Protect skin tones")
                    .on_hover_text("Keep skin-coloured areas from being squeezed or stretched");
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("preview {:.0} ms", st.carve_ms))
                        .small()
                        .color(MUTED),
                );
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add(primary_button("Apply"))
                            .on_hover_text("Apply at full size (Enter)")
                            .clicked()
                        {
                            done = Some(true);
                        }
                        if ui
                            .add(footer_button("Cancel"))
                            .on_hover_text("Leave the layer as it was (Esc)")
                            .clicked()
                        {
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
                a11y_name(&resp, "Content-Aware Scale preview");
                let canvas = st.canvas;
                // Fit the canvas and the box (as it was when a drag began,
                // so the view holds still under the pointer).
                let [vx0, vy0, vx1, vy1] = st.view_bounds();
                let disp = ((avail.width() - 64.0) / (vx1 - vx0))
                    .min((avail.height() - 64.0) / (vy1 - vy0))
                    .max(0.01);
                let view = egui::Rect::from_center_size(
                    avail.center(),
                    egui::vec2((vx1 - vx0) * disp, (vy1 - vy0) * disp),
                );
                let to_screen = |c: (f32, f32)| {
                    egui::pos2(view.min.x + (c.0 - vx0) * disp, view.min.y + (c.1 - vy0) * disp)
                };
                let to_canvas = |s: Pos2| (vx0 + (s.x - view.min.x) / disp, vy0 + (s.y - view.min.y) / disp);
                let img = egui::Rect::from_min_max(
                    to_screen((canvas.x as f32, canvas.y as f32)),
                    to_screen((canvas.right() as f32, canvas.bottom() as f32)),
                );
                // Pointer: grab, drag, release.
                let (pressed, down, shift, pointer) = ctx.input(|i| {
                    (
                        i.pointer.primary_pressed(),
                        i.pointer.primary_down(),
                        i.modifiers.shift,
                        i.pointer.interact_pos(),
                    )
                });
                let box_rect = egui::Rect::from_min_max(
                    to_screen((st.bx[0], st.bx[1])),
                    to_screen((st.bx[2], st.bx[3])),
                );
                st.screen_box = box_rect;
                st.hover = resp.hover_pos().and_then(|p| CasState::grip_at(box_rect, p));
                if pressed && resp.hovered() {
                    if let (Some(g), Some(p)) = (st.hover, pointer) {
                        st.drag = Some((g, st.bx, to_canvas(p)));
                    }
                }
                let mut released = false;
                if let Some((g, start, press)) = st.drag {
                    match pointer.filter(|_| down) {
                        Some(p) => {
                            st.bx = CasState::dragged(start, g, press, to_canvas(p), shift);
                            ctx.request_repaint();
                        }
                        None => {
                            st.drag = None;
                            released = true;
                        }
                    }
                }
                // Carve: always when idle; while dragging only if it is fast.
                let doc = self.editor.doc();
                if st.drag.is_none() || st.carve_ms < LIVE_MS || released {
                    st.carve(ctx, doc);
                } else {
                    st.stale = true;
                }
                if st.back_tex.is_none() {
                    let image = liquify::to_image(&st.backdrop);
                    st.back_tex = Some(ctx.load_texture("cas-back", image, egui::TextureOptions::LINEAR));
                }
                let p = ui.painter_at(avail);
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                if let Some(t) = &st.back_tex {
                    p.image(t.id(), img, uv, Color32::WHITE);
                }
                let box_rect = egui::Rect::from_min_max(
                    to_screen((st.bx[0], st.bx[1])),
                    to_screen((st.bx[2], st.bx[3])),
                );
                if let Some(t) = &st.layer_tex {
                    let a = (st.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                    p.image(t.id(), box_rect, uv, Color32::from_white_alpha(a));
                }
                p.rect_stroke(img, 0.0, Stroke::new(1.0, LINE));
                // The box and its eight handles.
                p.rect_stroke(box_rect, 0.0, Stroke::new(2.0, Color32::from_black_alpha(140)));
                p.rect_stroke(box_rect, 0.0, Stroke::new(1.0, Color32::WHITE));
                let active = st.drag.map(|d| d.0).or(st.hover);
                let (l, r, t, b) = (box_rect.min.x, box_rect.max.x, box_rect.min.y, box_rect.max.y);
                let (mx, my) = ((l + r) / 2.0, (t + b) / 2.0);
                let handles = [
                    (l, t, true, false, true, false),
                    (mx, t, false, false, true, false),
                    (r, t, false, true, true, false),
                    (r, my, false, true, false, false),
                    (r, b, false, true, false, true),
                    (mx, b, false, false, false, true),
                    (l, b, true, false, false, true),
                    (l, my, true, false, false, false),
                ];
                for (x, y, gl, gr, gt, gb) in handles {
                    let g = Grip {
                        left: gl,
                        right: gr,
                        top: gt,
                        bottom: gb,
                    };
                    let hot = active == Some(g);
                    let h = egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(8.0, 8.0));
                    p.rect_filled(h.expand(1.0), 0.0, Color32::from_black_alpha(160));
                    p.rect_filled(h, 0.0, if hot { ACCENT } else { Color32::WHITE });
                }
                if let Some(g) = active {
                    ctx.set_cursor_icon(match (g.left || g.right, g.top || g.bottom) {
                        (false, false) => egui::CursorIcon::Move,
                        (true, false) => egui::CursorIcon::ResizeHorizontal,
                        (false, true) => egui::CursorIcon::ResizeVertical,
                        _ if (g.left && g.top) || (g.right && g.bottom) => egui::CursorIcon::ResizeNwSe,
                        _ => egui::CursorIcon::ResizeNeSw,
                    });
                }
                if st.stale {
                    ctx.request_repaint();
                }
            });

        match done {
            Some(true) => self.finish_cas(st),
            Some(false) => self.status = "Content-Aware Scale cancelled".into(),
            None => self.cas = Some(st),
        }
    }

    /// Debug tokens: `cas:open` opens the workspace on the active layer;
    /// `cas:w=PCT` and `cas:h=PCT` set the size about the centre;
    /// `cas:box=X0:Y0:X1:Y1` sets the target box (canvas px);
    /// `cas:amount=N` (0–100); `cas:protect=none|selection|NAME`;
    /// `cas:skin` ticks Protect skin tones; `cas:commit`, `cas:cancel`.
    pub(crate) fn debug_cas(&mut self, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("cas:") else {
            return false;
        };
        if self.cas.is_none() && !matches!(rest, "commit" | "cancel") {
            self.open_cas();
        }
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        if verb == "commit" {
            if let Some(st) = self.cas.take() {
                self.finish_cas(st);
            }
            return true;
        }
        if verb == "cancel" {
            self.cas = None;
            return true;
        }
        let saved: Vec<String> = self
            .editor
            .doc()
            .saved_selections
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let Some(st) = self.cas.as_mut() else {
            return true;
        };
        match (verb, nums.as_slice()) {
            ("open", _) => {}
            ("w", [v]) => {
                let (_, hp) = st.percent();
                st.set_percent(*v, hp);
            }
            ("h", [v]) => {
                let (wp, _) = st.percent();
                st.set_percent(wp, *v);
            }
            ("box", [x0, y0, x1, y1]) => st.bx = [*x0, *y0, *x1, *y1],
            ("amount", [v]) => st.amount = v.clamp(0.0, 100.0),
            ("skin", _) => st.skin = true,
            ("protect", _) => {
                st.protect = match arg {
                    "none" => Protect::None,
                    "selection" => Protect::Selection,
                    name => match saved.iter().position(|s| s == name) {
                        Some(i) => Protect::Channel(i),
                        None => return false,
                    },
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
    use lumenply_tiles::Rgba;

    /// A 120 × 60 document: flat grey left half, a striped right half.
    fn launch_photo() -> App {
        let mut r = Raster::filled(120, 60, Rgba::new(0.4, 0.4, 0.4, 1.0));
        for y in 0..60 {
            for x in 60..120 {
                let v = if (x / 3 + y / 5) % 2 == 0 { 0.9 } else { 0.1 };
                r.set(x, y, Rgba::new(v, v * 0.5, 0.2, 1.0));
            }
        }
        let mut doc = Document::new(120, 60);
        let id = doc.add_pixel_layer("Photo");
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        let id = app.editor.doc().layers()[0].id;
        app.set_active(Some(id));
        app
    }

    #[test]
    fn commit_scales_the_layer_in_one_undo_step() {
        let mut app = launch_photo();
        let id = app.active.unwrap();
        let steps = app.editor.history().len();
        app.run_menu_action("content-aware-scale");
        let st = app.cas.as_mut().expect("open");
        assert_eq!(st.percent(), (100.0, 100.0));
        // 70 % wide about the centre: 84 px, from x = 18.
        st.set_percent(70.0, 100.0);
        assert_eq!(st.target(), (18, 0, 84, 60));
        assert!(app.debug_cas("cas:commit"));
        assert!(app.cas.is_none());
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last(), Some(&"Content-Aware Scale"));
        let px = app.editor.doc().layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.content_bounds(), Some(Rect::new(18, 0, 84, 60)));
        // The stripes moved left by 36 px (to x = 42), untouched.
        let a = px.get_pixel(42, 2);
        assert!((a.r - 0.9).abs() < 1e-3 && (a.g - 0.45).abs() < 1e-3, "{a:?}");
        let b = px.get_pixel(45, 2);
        assert!((b.r - 0.1).abs() < 1e-3, "{b:?}");
    }

    #[test]
    fn cancel_and_an_unchanged_box_change_nothing() {
        let mut app = launch_photo();
        let steps = app.editor.history().len();
        app.run_menu_action("content-aware-scale");
        assert!(app.debug_cas("cas:commit"), "unchanged box");
        assert_eq!(app.editor.history().len(), steps);
        app.run_menu_action("content-aware-scale");
        app.debug_cas("cas:w=50");
        app.debug_cas("cas:cancel");
        assert!(app.cas.is_none());
        assert_eq!(app.editor.history().len(), steps);
    }

    #[test]
    fn handles_resize_from_the_opposite_edge_and_shift_keeps_proportions() {
        let start = [0.0, 0.0, 100.0, 50.0];
        let right = Grip {
            right: true,
            ..Grip::default()
        };
        assert_eq!(
            CasState::dragged(start, right, (100.0, 25.0), (70.0, 30.0), false),
            [0.0, 0.0, 70.0, 50.0]
        );
        let tl = Grip {
            left: true,
            top: true,
            ..Grip::default()
        };
        // Shift: the larger relative change wins, the ratio stays 2:1.
        // 60 % as wide and 90 % as tall: 90 % both ways, from the
        // bottom-right corner.
        assert_eq!(
            CasState::dragged(start, tl, (0.0, 0.0), (40.0, 5.0), true),
            [10.0, 5.0, 100.0, 50.0]
        );
        assert_eq!(
            CasState::dragged(start, tl, (0.0, 0.0), (40.0, 5.0), false),
            [40.0, 5.0, 100.0, 50.0]
        );
        let mv = Grip::default();
        assert_eq!(
            CasState::dragged(start, mv, (10.0, 10.0), (15.0, 7.0), false),
            [5.0, -3.0, 105.0, 47.0]
        );
        // Screen hit-testing: corners, edges, inside, outside.
        let r = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(300.0, 200.0));
        assert_eq!(CasState::grip_at(r, egui::pos2(102.0, 98.0)), Some(tl));
        assert_eq!(CasState::grip_at(r, egui::pos2(296.0, 150.0)), Some(right));
        assert_eq!(CasState::grip_at(r, egui::pos2(200.0, 150.0)), Some(mv));
        assert_eq!(CasState::grip_at(r, egui::pos2(50.0, 150.0)), None);
    }

    #[test]
    fn protect_offers_the_selection_and_saved_channels() {
        let mut app = launch_photo();
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(0, 0, 30, 60))),
        });
        app.run_menu_action("content-aware-scale");
        let st = app.cas.as_mut().unwrap();
        assert_eq!(st.protect, Protect::None, "nothing is protected until picked");
        st.set_percent(80.0, 100.0);
        assert!(st.command(app.editor.doc()).unwrap().protect.is_none());
        assert!(app.debug_cas("cas:protect=selection"));
        let st = app.cas.as_mut().unwrap();
        let cmd = st.command(app.editor.doc()).unwrap();
        let m = cmd.protect.as_ref().expect("the selection protects");
        assert_eq!(m.to_dense(Rect::new(29, 10, 2, 1)), vec![1.0, 0.0]);
        assert_eq!((cmd.new_w, cmd.new_h, cmd.origin), (96, 60, Some((12, 0))));
        assert!(app.debug_cas("cas:protect=none"));
        assert_eq!(app.cas.as_ref().unwrap().protect, Protect::None);
        assert!(!app.debug_cas("cas:protect=No such channel"));
    }

    #[test]
    fn blocked_where_it_cannot_work() {
        let mut app = launch_photo();
        assert_eq!(app.action_block("content-aware-scale"), None);
        app.add_adjustment(lumenply_doc::Adjustment::Invert);
        assert_eq!(
            app.action_block("content-aware-scale"),
            Some("Select a pixel layer first")
        );
        app.run_menu_action("content-aware-scale");
        assert!(app.cas.is_none());
    }

    /// Runs one frame of the whole app with these input events.
    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    fn button(p: Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            },
        ]
    }

    #[test]
    fn dragging_the_right_handle_narrows_the_box_from_the_left_edge() {
        let mut app = launch_photo();
        app.run_menu_action("content-aware-scale");
        let ctx = crate::a11y_tests::ctx();
        frame(&mut app, &ctx, vec![]);
        let b = app.cas.as_ref().unwrap().screen_box;
        assert!(b.width() > 100.0, "the box is on screen: {b:?}");
        let disp = b.width() / 120.0;
        let from = egui::pos2(b.max.x, b.center().y);
        let to = from - egui::vec2(36.0 * disp, 0.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(from)]);
        frame(&mut app, &ctx, button(from, true));
        for k in 1..=4 {
            let p = from + (to - from) * (k as f32 / 4.0);
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(p)]);
        }
        frame(&mut app, &ctx, button(to, false));
        frame(&mut app, &ctx, vec![]);
        let st = app.cas.as_ref().unwrap();
        assert_eq!(st.target(), (0, 0, 84, 60), "{:?}", st.bx);
        // Enter commits.
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.cas.is_none());
        assert_eq!(app.editor.history().last(), Some(&"Content-Aware Scale"));
        let px = app.editor.doc().layers()[0].pixels().unwrap();
        assert_eq!(px.content_bounds(), Some(Rect::new(0, 0, 84, 60)));
    }

    #[test]
    fn every_control_in_the_workspace_has_a_spoken_name() {
        let mut app = launch_photo();
        app.run_menu_action("content-aware-scale");
        app.debug_cas("cas:w=80");
        let ctx = crate::a11y_tests::ctx();
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.cas.is_some(), "still open");
        assert_eq!(missing, Vec::<String>::new());
    }
}
