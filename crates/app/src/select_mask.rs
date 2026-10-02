//! Select ▸ Select and Mask…: Photoshop's refine-edge workspace. It
//! replaces the editor UI while open: a live preview of the refined
//! selection (overlay, on black, on white, black & white, marching ants)
//! computed on a reduced copy, the Edge Detection / Global Refinements /
//! Output panel, and a Refine Edge brush that widens the band where it
//! paints (Alt restores). OK applies `RefineSelection` at full resolution
//! as one undo step.

use super::*;
use lumenply_core::refine::{
    edge_matte, global_refine, paint_band, refine_band, RefineDab, RefineOutput, RefineParams,
    RefineSelection,
};
use rayon::prelude::*;

/// Longest preview side in pixels.
const PREVIEW_MAX: u32 = 2048;
/// Label column of the panel's rows.
const LABEL: f32 = 92.0;

/// How the preview shows the refined selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaskView {
    Overlay,
    OnBlack,
    OnWhite,
    BlackWhite,
    Ants,
}

impl MaskView {
    const ALL: [MaskView; 5] = [
        MaskView::Overlay,
        MaskView::OnBlack,
        MaskView::OnWhite,
        MaskView::BlackWhite,
        MaskView::Ants,
    ];

    fn name(self) -> &'static str {
        match self {
            MaskView::Overlay => "Overlay",
            MaskView::OnBlack => "On black",
            MaskView::OnWhite => "On white",
            MaskView::BlackWhite => "Black & white",
            MaskView::Ants => "Marching ants",
        }
    }

    fn token(self) -> &'static str {
        match self {
            MaskView::Overlay => "overlay",
            MaskView::OnBlack => "black",
            MaskView::OnWhite => "white",
            MaskView::BlackWhite => "bw",
            MaskView::Ants => "ants",
        }
    }
}

/// The settings remembered between openings (as Photoshop does).
#[derive(Clone, Copy)]
pub(crate) struct SelectMaskPrefs {
    params: RefineParams,
    view: MaskView,
    opacity: f32,
    output: RefineOutput,
}

impl Default for SelectMaskPrefs {
    fn default() -> Self {
        SelectMaskPrefs {
            params: RefineParams {
                radius: 8.0,
                ..RefineParams::default()
            },
            view: MaskView::Overlay,
            opacity: 0.5,
            output: RefineOutput::Selection,
        }
    }
}

/// What the cached edge matte was computed from.
#[derive(Clone, Copy, PartialEq)]
struct EdgeKey {
    radius: f32,
    smart: bool,
    dabs: usize,
    sample_all: bool,
}

pub(crate) struct SelectMaskState {
    layer: Option<LayerId>,
    layer_name: String,
    layer_pixel: bool,
    pub(crate) params: RefineParams,
    pub(crate) view: MaskView,
    /// Overlay strength, 0..=1.
    pub(crate) opacity: f32,
    /// Show only the band Edge Detection works in (J).
    pub(crate) show_edge: bool,
    pub(crate) output: RefineOutput,
    /// Fit the edge to the composite rather than the active layer.
    pub(crate) sample_all: bool,
    /// Refine Edge brush diameter in document pixels.
    pub(crate) brush_size: f32,
    /// Brush strokes in document pixels (Cmd+Z drops the last).
    pub(crate) strokes: Vec<Vec<RefineDab>>,
    canvas: Rect,
    /// Preview pixels are `factor` document pixels square.
    factor: u32,
    /// The composite, reduced (what the preview shows).
    shown: Raster,
    /// The active pixel layer, reduced (what the edge is fitted to unless
    /// sampling all layers).
    layer_img: Option<Raster>,
    /// The selection, reduced.
    cov: Vec<f32>,
    edge: Vec<f32>,
    band: Vec<bool>,
    edge_key: Option<EdgeKey>,
    matte: Vec<f32>,
    matte_key: Option<(EdgeKey, RefineParams)>,
    generation: u64,
    tex: Option<egui::TextureHandle>,
    tex_key: Option<(MaskView, u32, bool, u64)>,
    /// Milliseconds the last preview refine took.
    pub(crate) last_ms: f32,
    /// Document position of the last dab while the brush is down.
    last_dab: Option<(f32, f32)>,
    /// Preview zoom over "fit" (1 fits the window) and pan in points.
    pub(crate) zoom: f32,
    pan: egui::Vec2,
    /// A pending "show document point (x, y) at this many screen points
    /// per pixel" (debug token).
    focus: Option<(f32, f32, f32)>,
}

/// `src` box-filtered down by a whole `f`.
fn shrink(src: &Raster, f: u32) -> Raster {
    if f <= 1 {
        return src.clone();
    }
    let (w, h) = (src.width.div_ceil(f), src.height.div_ceil(f));
    let mut out = Raster::new(w, h);
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            for (x, p) in row.iter_mut().enumerate() {
                let x = x as u32;
                let mut acc = [0.0f32; 4];
                let mut n = 0.0;
                for sy in y * f..((y + 1) * f).min(src.height) {
                    for sx in x * f..((x + 1) * f).min(src.width) {
                        let q = src.get(sx, sy);
                        acc[0] += q.r;
                        acc[1] += q.g;
                        acc[2] += q.b;
                        acc[3] += q.a;
                        n += 1.0;
                    }
                }
                *p = lumenply_tiles::Rgba::new(acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n);
            }
        });
    out
}

/// A `w`×`h` coverage buffer box-filtered down by a whole `f`.
fn shrink_cov(src: &[f32], w: usize, h: usize, f: usize) -> Vec<f32> {
    if f <= 1 {
        return src.to_vec();
    }
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let mut out = vec![0f32; sw * sh];
    out.par_chunks_mut(sw).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (mut acc, mut n) = (0f32, 0f32);
            for sy in y * f..((y + 1) * f).min(h) {
                for sx in x * f..((x + 1) * f).min(w) {
                    acc += src[sy * w + sx];
                    n += 1.0;
                }
            }
            *o = acc / n;
        }
    });
    out
}

impl SelectMaskState {
    fn size(&self) -> (usize, usize) {
        (self.shown.width as usize, self.shown.height as usize)
    }

    fn scale(&self) -> f32 {
        1.0 / self.factor as f32
    }

    fn dabs(&self) -> Vec<RefineDab> {
        self.strokes.iter().flatten().copied().collect()
    }

    fn sample_source(&self, layer: Option<LayerId>) -> SampleSource {
        match layer {
            Some(id) if !self.sample_all && self.layer_pixel => SampleSource::Layer(id),
            _ => SampleSource::Merged,
        }
    }

    /// Brings the preview matte up to date; true when it changed.
    pub(crate) fn update(&mut self) -> bool {
        let key = EdgeKey {
            radius: self.params.radius,
            smart: self.params.smart_radius,
            dabs: self.strokes.iter().map(Vec::len).sum(),
            sample_all: self.sample_all,
        };
        let t = std::time::Instant::now();
        let (w, h) = self.size();
        if self.edge_key != Some(key) {
            let dabs = self.dabs();
            let band = (!dabs.is_empty()).then(|| {
                paint_band(
                    &dabs,
                    w,
                    h,
                    (self.canvas.x as f32, self.canvas.y as f32),
                    self.scale(),
                )
            });
            let img = match &self.layer_img {
                Some(l) if !self.sample_all => l,
                _ => &self.shown,
            };
            let edge_only = RefineParams {
                radius: self.params.radius,
                smart_radius: self.params.smart_radius,
                ..RefineParams::default()
            };
            self.edge = edge_matte(img, &self.cov, band.as_deref(), &edge_only, self.scale());
            self.band = refine_band(
                &self.cov,
                band.as_deref(),
                w,
                h,
                self.params.radius * self.scale(),
            );
            self.edge_key = Some(key);
        }
        let mkey = (key, self.params);
        if self.matte_key == Some(mkey) {
            return false;
        }
        self.matte = global_refine(self.edge.clone(), w, h, &self.params, self.scale());
        self.matte_key = Some(mkey);
        self.generation += 1;
        self.last_ms = t.elapsed().as_secs_f32() * 1000.0;
        true
    }

    /// The preview in the current view mode.
    fn image(&self) -> egui::ColorImage {
        let (w, h) = self.size();
        let (light, dark) = (
            lumenply_io::srgb_to_linear_f(0.80),
            lumenply_io::srgb_to_linear_f(0.66),
        );
        let enc = lumenply_io::linear_to_srgb;
        let ants = self.view == MaskView::Ants;
        let inside = |x: usize, y: usize| self.matte[y * w + x] >= 0.5;
        let mut px = vec![Color32::BLACK; w * h];
        px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let p = self.shown.pixels[i];
                let a = self.matte[i].clamp(0.0, 1.0);
                // The composite over the checkerboard, linear.
                let bg = if (x / 8 + y / 8) % 2 == 0 { light } else { dark };
                let k = 1.0 - p.a.clamp(0.0, 1.0);
                let base = [p.r + bg * k, p.g + bg * k, p.b + bg * k];
                let mut c = match self.view {
                    MaskView::Overlay => {
                        let t = (1.0 - a) * self.opacity;
                        let red = [0.85, 0.02, 0.02];
                        [0, 1, 2].map(|ch| base[ch] * (1.0 - t) + red[ch] * t)
                    }
                    MaskView::OnBlack => [0, 1, 2].map(|ch| p_ch(p, ch) * a),
                    MaskView::OnWhite => [0, 1, 2].map(|ch| p_ch(p, ch) * a + (1.0 - p.a * a)),
                    MaskView::BlackWhite => [a; 3],
                    MaskView::Ants => base,
                };
                if self.view == MaskView::BlackWhite {
                    // A mask reads as its value, not as light.
                    c = [srgb_inv(a); 3];
                }
                if self.show_edge && !self.band[i] {
                    c = c.map(|v| v * 0.12);
                }
                *out = Color32::from_rgb(enc(c[0]), enc(c[1]), enc(c[2]));
                if ants && inside(x, y) {
                    let edge = x == 0
                        || y == 0
                        || x + 1 == w
                        || y + 1 == h
                        || !inside(x - 1, y)
                        || !inside(x + 1, y)
                        || !inside(x, y - 1)
                        || !inside(x, y + 1);
                    if edge {
                        *out = if (x + y) / 3 % 2 == 0 {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        };
                    }
                }
            }
        });
        egui::ColorImage {
            size: [w, h],
            pixels: px,
        }
    }
}

/// One premultiplied channel.
fn p_ch(p: lumenply_tiles::Rgba, ch: usize) -> f32 {
    [p.r, p.g, p.b][ch]
}

/// The linear value whose sRGB encoding is `v` (so a mask value shows as
/// that grey level).
fn srgb_inv(v: f32) -> f32 {
    lumenply_io::srgb_to_linear_f(v)
}

impl App {
    /// Opens the workspace on the active selection (the action registry
    /// blocks it without one).
    pub(crate) fn open_select_mask(&mut self) {
        let doc = self.editor.doc();
        let Some(sel) = doc.selection.as_ref() else {
            self.status = "Make a selection first".into();
            return;
        };
        let canvas = doc.canvas();
        let factor = canvas.w.max(canvas.h).div_ceil(PREVIEW_MAX).max(1);
        let full = lumenply_render::composite_raster(doc);
        let shown = shrink(&full, factor);
        let layer = self.active.and_then(|id| doc.layer(id));
        let layer_pixel = layer.is_some_and(|l| l.pixels().is_some());
        let layer_img = layer
            .and_then(|l| l.pixels())
            .map(|s| shrink(&s.to_raster(canvas), factor));
        let cov = shrink_cov(
            &sel.coverage.to_dense(canvas),
            canvas.w as usize,
            canvas.h as usize,
            factor as usize,
        );
        let prefs = self.select_mask_prefs;
        let mut output = prefs.output;
        if (output == RefineOutput::LayerMask && layer.is_none()) || (output.makes_layer() && !layer_pixel) {
            output = RefineOutput::Selection;
        }
        let mut params = prefs.params;
        if params.decontaminate && !output.makes_layer() {
            params.decontaminate = false;
        }
        let n = shown.pixels.len();
        self.select_mask = Some(Box::new(SelectMaskState {
            layer: self.active,
            layer_name: layer.map_or("Composite".into(), |l| l.name.clone()),
            layer_pixel,
            params,
            view: prefs.view,
            opacity: prefs.opacity,
            show_edge: false,
            output,
            sample_all: !layer_pixel,
            brush_size: (canvas.w.min(canvas.h) as f32 / 25.0).clamp(10.0, 300.0).round(),
            strokes: Vec::new(),
            canvas,
            factor,
            shown,
            layer_img,
            cov,
            edge: Vec::new(),
            band: vec![false; n],
            edge_key: None,
            matte: Vec::new(),
            matte_key: None,
            generation: 0,
            tex: None,
            tex_key: None,
            last_ms: 0.0,
            last_dab: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            focus: None,
        }));
    }

    /// Applies the refine at full resolution and closes the workspace.
    fn finish_select_mask(&mut self, st: SelectMaskState) {
        self.select_mask_prefs = SelectMaskPrefs {
            params: st.params,
            view: st.view,
            opacity: st.opacity,
            output: st.output,
        };
        let cmd = RefineSelection {
            params: st.params,
            brush: st.dabs(),
            output: st.output,
            layer: st.layer,
            sample: st.sample_source(st.layer),
        };
        let before = self.editor.history().len();
        let t = std::time::Instant::now();
        self.run(&cmd);
        let secs = t.elapsed().as_secs_f32();
        if self.editor.history().len() == before {
            return; // `run` reported the error
        }
        if st.output.makes_layer() {
            // The refined copy sits right above its (now hidden) source.
            let next = st.layer.and_then(|id| {
                let doc = self.editor.doc();
                let list = match doc.parent_of(id) {
                    None => doc.layers(),
                    Some(p) => doc.layer(p)?.children()?,
                };
                let i = list.iter().position(|l| l.id == id)?;
                list.get(i + 1).map(|l| l.id)
            });
            if next.is_some() {
                self.set_active(next);
            }
        }
        self.status = format!(
            "Select and Mask: {} in {secs:.2} s",
            st.output.name().to_lowercase()
        );
    }

    /// The workspace; replaces the editor UI while open.
    pub(crate) fn select_mask_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.select_mask.take() else {
            return;
        };
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
                    st.strokes.pop();
                } else if !i.modifiers.command {
                    if i.key_pressed(Key::J) {
                        st.show_edge = !st.show_edge;
                    }
                    if i.key_pressed(Key::F) {
                        let k = MaskView::ALL.iter().position(|v| *v == st.view).unwrap_or(0);
                        st.view = MaskView::ALL[(k + 1) % MaskView::ALL.len()];
                    }
                }
                if i.key_pressed(Key::OpenBracket) {
                    st.brush_size = (st.brush_size / 1.15).max(2.0).round();
                }
                if i.key_pressed(Key::CloseBracket) {
                    st.brush_size = (st.brush_size * 1.15).min(1000.0).round();
                }
            });
        }

        egui::TopBottomPanel::top("select-mask-bar")
            .frame(bar_frame())
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Select and Mask").strong().color(TEXT));
                    ui.label(RichText::new(&st.layer_name).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                "Paint to refine the edge · Alt restores · [ ] size · J edge · F view · wheel zooms",
                            )
                            .color(MUTED),
                        );
                    });
                });
            });

        egui::SidePanel::right("select-mask-props")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("select-mask-panel")
                    .max_height(ui.available_height() - 64.0)
                    .show(ui, |ui| self.select_mask_panel(ui, &mut st));
                a11y_scroll(ui.ctx(), &scroll, "Select and Mask settings");
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    let (w, h) = st.size();
                    ui.label(
                        RichText::new(format!("Preview {w}×{h} · {:.0} ms", st.last_ms))
                            .small()
                            .color(MUTED),
                    );
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

        st.update();
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.canvas_color()))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(avail, Sense::click_and_drag());
                a11y_name(&resp, "Select and Mask preview");
                let (cw, ch) = (st.canvas.w as f32, st.canvas.h as f32);
                let fit = ((avail.width() - 48.0) / cw)
                    .min((avail.height() - 48.0) / ch)
                    .max(0.01);
                // Wheel zooms at the pointer; Space-drag or the middle
                // button pans; 0 fits, 1 shows actual pixels.
                let (scroll, space, middle, keys) = ctx.input(|i| {
                    (
                        i.raw_scroll_delta.y + i.zoom_delta().ln() * 300.0,
                        i.key_down(Key::Space),
                        i.pointer.middle_down(),
                        (i.key_pressed(Key::Num0), i.key_pressed(Key::Num1)),
                    )
                });
                if !ctx.wants_keyboard_input() {
                    if keys.0 {
                        st.zoom = 1.0;
                        st.pan = egui::Vec2::ZERO;
                    }
                    if keys.1 {
                        st.zoom = 1.0 / fit;
                        st.pan = egui::Vec2::ZERO;
                    }
                }
                if let Some(pos) = resp.hover_pos().filter(|_| scroll != 0.0) {
                    let old = fit * st.zoom;
                    st.zoom = (st.zoom * (scroll / 400.0).exp()).clamp(0.5, 64.0 / fit);
                    let new = fit * st.zoom;
                    // Keep the point under the pointer still.
                    let c = avail.center() + st.pan;
                    st.pan += (pos - c) * (1.0 - new / old);
                }
                if let Some((z, x, y)) = st.focus.take() {
                    st.zoom = z / fit;
                    st.pan = -egui::vec2(
                        (x - st.canvas.x as f32 - cw / 2.0) * z,
                        (y - st.canvas.y as f32 - ch / 2.0) * z,
                    );
                }
                let panning = space || middle;
                if panning && resp.dragged() {
                    st.pan += resp.drag_delta();
                }
                let disp = fit * st.zoom;
                let img =
                    egui::Rect::from_center_size(avail.center() + st.pan, egui::vec2(cw * disp, ch * disp));
                let key = (st.view, st.opacity.to_bits(), st.show_edge, st.generation);
                if st.tex_key != Some(key) || st.tex.is_none() {
                    let image = st.image();
                    match &mut st.tex {
                        Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            st.tex =
                                Some(ctx.load_texture("select-mask", image, egui::TextureOptions::LINEAR))
                        }
                    }
                    st.tex_key = Some(key);
                }
                let p = ui.painter_at(avail);
                if let Some(t) = &st.tex {
                    // The reduced copy's last row and column may cover less
                    // than `factor` pixels: show only the canvas part.
                    let f = st.factor as f32;
                    let uv = egui::Rect::from_min_max(
                        egui::pos2(0.0, 0.0),
                        egui::pos2(
                            cw / (st.shown.width as f32 * f),
                            ch / (st.shown.height as f32 * f),
                        ),
                    );
                    p.image(t.id(), img, uv, Color32::WHITE);
                }
                p.rect_stroke(img, 0.0, Stroke::new(1.0, LINE));
                let down = resp.is_pointer_button_down_on() && !panning;
                if let Some(pos) = resp.hover_pos().or(resp.interact_pointer_pos()) {
                    let c = (
                        st.canvas.x as f32 + (pos.x - img.min.x) / disp,
                        st.canvas.y as f32 + (pos.y - img.min.y) / disp,
                    );
                    let r = st.brush_size / 2.0 * disp;
                    p.circle_stroke(pos, r, Stroke::new(2.5, Color32::from_black_alpha(140)));
                    p.circle_stroke(pos, r, Stroke::new(1.0, Color32::WHITE));
                    ctx.set_cursor_icon(if panning {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Crosshair
                    });
                    if down {
                        let erase = ctx.input(|i| i.modifiers.alt);
                        st.brush_to(c, erase);
                        ctx.request_repaint();
                    }
                }
                if !down {
                    st.last_dab = None;
                }
            });

        match done {
            Some(true) => {
                self.select_mask = None;
                self.finish_select_mask(*st);
            }
            Some(false) => {
                self.status = "Select and Mask cancelled".into();
                self.select_mask = None;
            }
            None => self.select_mask = Some(st),
        }
    }

    /// The right-hand panel's sections.
    fn select_mask_panel(&self, ui: &mut egui::Ui, st: &mut SelectMaskState) {
        let opts = RowOpts {
            label_w: LABEL,
            ..RowOpts::default()
        };
        section_title(ui, "VIEW");
        ui.horizontal(|ui| {
            row_label(ui, "View", LABEL);
            ui.spacing_mut().combo_width = ui.available_width();
            let r = egui::ComboBox::from_id_salt("select-mask-view")
                .selected_text(st.view.name())
                .show_ui(ui, |ui| {
                    popup_style(ui);
                    for v in MaskView::ALL {
                        ui.selectable_value(&mut st.view, v, v.name());
                    }
                });
            a11y_name(&r.response, "View");
        });
        ui.add_enabled_ui(st.view == MaskView::Overlay, |ui| {
            slider_row_scaled_w(ui, "Opacity", &mut st.opacity, 0.0..=1.0, 100.0, "%", LABEL);
        });
        check(ui, &mut st.show_edge, "Show edge (J)")
            .on_hover_text("Show only the band the edge is refined in");
        ui.add_enabled_ui(st.layer_pixel, |ui| {
            check(ui, &mut st.sample_all, "Sample all layers")
                .on_hover_text("Fit the edge to the whole picture instead of the active layer")
                .on_disabled_hover_text("The active layer has no pixels: the whole picture is sampled");
        });

        section_title(ui, "EDGE DETECTION");
        let log = RowOpts { log: true, ..opts };
        slider_row_ex(ui, "Radius", &mut st.params.radius, 0.0..=250.0, " px", log);
        check(ui, &mut st.params.smart_radius, "Smart radius")
            .on_hover_text("Narrow the band where the edge is hard, keep it wide where it is soft");

        section_title(ui, "REFINE EDGE BRUSH");
        slider_row_ex(ui, "Size", &mut st.brush_size, 2.0..=1000.0, " px", log);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Paint over hair; Alt restores")
                    .small()
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!st.strokes.is_empty(), egui::Button::new("Clear"))
                    .on_hover_text("Remove every brush stroke")
                    .clicked()
                {
                    st.strokes.clear();
                }
            });
        });

        section_title(ui, "GLOBAL REFINEMENTS");
        slider_row_ex(ui, "Smooth", &mut st.params.smooth, 0.0..=100.0, "", opts);
        slider_row_ex(ui, "Feather", &mut st.params.feather, 0.0..=250.0, " px", log);
        slider_row_ex(ui, "Contrast", &mut st.params.contrast, 0.0..=100.0, "%", opts);
        slider_row_ex(
            ui,
            "Shift edge",
            &mut st.params.shift_edge,
            -100.0..=100.0,
            "%",
            opts,
        );

        section_title(ui, "OUTPUT");
        let had = st.params.decontaminate;
        ui.add_enabled_ui(st.layer_pixel, |ui| {
            check(ui, &mut st.params.decontaminate, "Decontaminate colours")
                .on_hover_text("Replace the background's colour fringe with nearby foreground colour")
                .on_disabled_hover_text("Needs a pixel layer to copy");
        });
        if st.params.decontaminate && !had && !st.output.makes_layer() {
            st.output = RefineOutput::NewLayerWithMask;
        }
        ui.add_enabled_ui(st.params.decontaminate, |ui| {
            slider_row_ex(
                ui,
                "Amount",
                &mut st.params.decontam_amount,
                0.0..=100.0,
                "%",
                opts,
            );
        });
        ui.horizontal(|ui| {
            row_label(ui, "Output to", LABEL);
            ui.spacing_mut().combo_width = ui.available_width();
            let r = egui::ComboBox::from_id_salt("select-mask-output")
                .selected_text(st.output.name())
                .show_ui(ui, |ui| {
                    popup_style(ui);
                    for o in RefineOutput::ALL {
                        let block = output_block(st, o);
                        ui.add_enabled_ui(block.is_none(), |ui| {
                            ui.selectable_value(&mut st.output, o, o.name())
                                .on_disabled_hover_text(block.unwrap_or_default());
                        });
                    }
                });
            a11y_name(&r.response, "Output to");
        });
        ui.add_space(4.0);
        if ui
            .button("Reset")
            .on_hover_text("Back to the default settings, no brush strokes")
            .clicked()
        {
            st.params = SelectMaskPrefs::default().params;
            st.strokes.clear();
        }
    }

    /// `refine:open` opens the workspace; `refine:ridge` first selects a
    /// rough polygon under the demo photo's ridge line; `refine:view=V`
    /// (overlay, black, white, bw, ants), `refine:radius=N`,
    /// `refine:smart`, `refine:smooth=N`, `refine:feather=N`,
    /// `refine:contrast=N`, `refine:shift=N`, `refine:opacity=N` (%),
    /// `refine:zoom=Z:X:Y` (Z screen points per pixel around X, Y),
    /// `refine:edge`, `refine:decontam`, `refine:output=selection|mask|
    /// layer|layer-mask`, `refine:brush=X:Y:R` (a dab, document pixels),
    /// `refine:ok` applies (timed in the status bar).
    pub(crate) fn debug_select_mask(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("refine:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let num = arg.parse::<f32>().ok();
        if verb == "ridge" {
            let d = self.editor.doc();
            let (w, h) = (d.width as f32, d.height as f32);
            let pts: Vec<(f32, f32)> = RIDGE
                .iter()
                .map(|&(x, y)| (x * w, y * h))
                .chain([(w, h), (0.0, h)])
                .collect();
            self.run(&SetSelection {
                selection: Some(Selection::polygon(&pts)),
            });
            return true;
        }
        if self.select_mask.is_none() {
            self.open_select_mask();
        }
        if verb == "ok" {
            if let Some(st) = self.select_mask.take() {
                self.finish_select_mask(*st);
                eprintln!("{}", self.status);
            }
            return true;
        }
        let Some(st) = self.select_mask.as_mut() else {
            return true;
        };
        match (verb, num) {
            ("open", _) => {}
            ("view", _) => {
                if let Some(v) = MaskView::ALL.into_iter().find(|v| v.token() == arg) {
                    st.view = v;
                }
            }
            ("radius", Some(v)) => st.params.radius = v,
            ("smart", _) => st.params.smart_radius = true,
            ("smooth", Some(v)) => st.params.smooth = v,
            ("feather", Some(v)) => st.params.feather = v,
            ("contrast", Some(v)) => st.params.contrast = v,
            ("shift", Some(v)) => st.params.shift_edge = v,
            ("opacity", Some(v)) => st.opacity = v / 100.0,
            ("edge", _) => st.show_edge = true,
            ("zoom", _) => {
                let v: Vec<f32> = arg.split(':').filter_map(|s| s.parse().ok()).collect();
                if let [z, x, y] = v[..] {
                    st.focus = Some((z, x, y));
                }
            }
            ("decontam", _) => {
                st.params.decontaminate = true;
                st.output = RefineOutput::NewLayerWithMask;
            }
            ("output", _) => {
                st.output = match arg {
                    "mask" => RefineOutput::LayerMask,
                    "layer" => RefineOutput::NewLayer,
                    "layer-mask" => RefineOutput::NewLayerWithMask,
                    _ => RefineOutput::Selection,
                }
            }
            ("brush", _) => {
                let v: Vec<f32> = arg.split(':').filter_map(|s| s.parse().ok()).collect();
                if let [x, y, r] = v[..] {
                    st.strokes.push(vec![RefineDab {
                        x,
                        y,
                        radius: r,
                        erase: false,
                    }]);
                }
            }
            _ => return false,
        }
        let t = std::time::Instant::now();
        st.update();
        if t.elapsed().as_millis() > 0 {
            eprintln!("select and mask preview: {:.0} ms", st.last_ms);
        }
        true
    }
}

/// A rough polygon under the demo photo's ridge line (fractions of the
/// canvas), for screenshots and timing.
const RIDGE: [(f32, f32); 14] = [
    (0.0, 0.50),
    (0.04, 0.48),
    (0.14, 0.43),
    (0.18, 0.425),
    (0.25, 0.47),
    (0.32, 0.52),
    (0.45, 0.545),
    (0.50, 0.525),
    (0.56, 0.545),
    (0.65, 0.53),
    (0.75, 0.545),
    (0.85, 0.515),
    (0.93, 0.495),
    (1.0, 0.51),
];

/// Why output `o` isn't available, if it isn't.
fn output_block(st: &SelectMaskState, o: RefineOutput) -> Option<&'static str> {
    match o {
        RefineOutput::Selection | RefineOutput::LayerMask if st.params.decontaminate => {
            Some("Decontaminated colours need a new layer")
        }
        RefineOutput::LayerMask if st.layer.is_none() => Some("Select a layer first"),
        RefineOutput::NewLayer | RefineOutput::NewLayerWithMask if !st.layer_pixel => {
            Some("Select a pixel layer first")
        }
        _ => None,
    }
}

impl SelectMaskState {
    /// Paint the Refine Edge brush to document point `c`, dabs spaced a
    /// quarter of the brush apart; a press starts a new stroke.
    fn brush_to(&mut self, c: (f32, f32), erase: bool) {
        let radius = self.brush_size / 2.0;
        let dab = |x: f32, y: f32| RefineDab { x, y, radius, erase };
        match self.last_dab {
            None => {
                self.strokes.push(vec![dab(c.0, c.1)]);
                self.last_dab = Some(c);
            }
            Some(last) => {
                let (dx, dy) = (c.0 - last.0, c.1 - last.1);
                let len = (dx * dx + dy * dy).sqrt();
                let step = (radius / 2.0).max(1.0);
                if len < step {
                    return;
                }
                let n = (len / step).floor() as usize;
                let stroke = self.strokes.last_mut().expect("a stroke is open");
                let mut at = last;
                for k in 1..=n {
                    let t = k as f32 * step / len;
                    at = (last.0 + dx * t, last.1 + dy * t);
                    stroke.push(dab(at.0, at.1));
                }
                self.last_dab = Some(at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    /// 96×64: red on the left, blue from x = 50, the selection stopping at
    /// x = 44.
    fn launch_tiny() -> App {
        let mut doc = Document::new(96, 64);
        let id = doc.add_pixel_layer("Background");
        let px = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..64 {
            for x in 0..96 {
                let c = if x < 50 {
                    Rgba::new(0.8, 0.1, 0.1, 1.0)
                } else {
                    Rgba::new(0.1, 0.2, 0.8, 1.0)
                };
                px.set_pixel(x, y, c);
            }
        }
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 44, 64)));
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app
    }

    fn sel_at(app: &App, x: i32, y: i32) -> f32 {
        app.editor.doc().selection.as_ref().map_or(0.0, |s| s.value(x, y))
    }

    #[test]
    fn the_workspace_refines_the_selection_with_one_undo_step() {
        let mut app = launch_tiny();
        let steps = app.editor.history().len();
        assert!(app.action_block("select-mask").is_none());
        app.run_menu_action("select-mask");
        let st = app.select_mask.as_mut().expect("open");
        assert_eq!(st.factor, 1, "small canvases preview at full size");
        st.params.radius = 10.0;
        assert!(st.update());
        // The preview already snaps to the colour edge at x = 50.
        assert_eq!(st.matte[20 * 96 + 49], 1.0);
        assert_eq!(st.matte[20 * 96 + 50], 0.0);
        assert!(!st.update(), "nothing changed, nothing recomputed");
        assert!(app.debug_select_mask(&egui::Context::default(), "refine:ok"));
        assert!(app.select_mask.is_none());
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last(), Some(&"Select and mask"));
        assert_eq!(sel_at(&app, 49, 20), 1.0);
        assert_eq!(sel_at(&app, 50, 20), 0.0);
    }

    #[test]
    fn cancel_leaves_the_document_alone_and_settings_are_remembered() {
        let mut app = launch_tiny();
        let steps = app.editor.history().len();
        app.run_menu_action("select-mask");
        let st = app.select_mask.as_mut().unwrap();
        st.params.feather = 3.0;
        st.view = MaskView::OnBlack;
        let st = app.select_mask.take().unwrap();
        app.finish_select_mask(*st);
        assert_eq!(app.editor.history().len(), steps + 1);
        app.undo();
        app.run_menu_action("select-mask");
        let st = app.select_mask.as_ref().unwrap();
        assert_eq!(st.params.feather, 3.0, "remembered");
        assert_eq!(st.view, MaskView::OnBlack);
        app.select_mask = None; // Cancel
        assert_eq!(app.editor.history().len(), steps);
        assert_eq!(sel_at(&app, 46, 20), 0.0);
    }

    #[test]
    fn the_brush_refines_where_it_paints_and_outputs_a_new_layer() {
        let mut app = launch_tiny();
        app.run_menu_action("select-mask");
        let st = app.select_mask.as_mut().unwrap();
        st.params.radius = 0.0;
        st.brush_size = 16.0;
        st.brush_to((46.0, 10.0), false);
        st.brush_to((46.0, 30.0), false);
        assert_eq!(st.strokes.len(), 1);
        assert_eq!(st.strokes[0].len(), 6, "a dab every 4 px");
        st.last_dab = None;
        st.update();
        // Refined under the stroke, untouched below it.
        assert_eq!(st.matte[20 * 96 + 49], 1.0);
        assert_eq!(st.matte[60 * 96 + 45], 0.0);
        st.output = RefineOutput::NewLayer;
        let src = app.active.unwrap();
        let st = app.select_mask.take().unwrap();
        app.finish_select_mask(*st);
        let doc = app.editor.doc();
        assert_eq!(doc.layers().len(), 2);
        assert!(!doc.layer(src).unwrap().visible);
        let copy = &doc.layers()[1];
        assert_eq!(app.active, Some(copy.id), "the cut-out becomes active");
        assert_eq!(copy.pixels().unwrap().get_pixel(49, 20).a, 1.0);
        assert_eq!(copy.pixels().unwrap().get_pixel(49, 60).a, 0.0);
        assert!(doc.selection.is_none());
    }

    #[test]
    fn the_action_needs_a_selection() {
        let mut app = launch_tiny();
        app.run(&SetSelection { selection: None });
        assert_eq!(app.action_block("select-mask"), Some("Make a selection first"));
        app.run_menu_action("select-mask");
        assert!(app.select_mask.is_none());
    }

    #[test]
    fn every_view_renders_and_every_control_has_a_spoken_name() {
        let mut app = launch_tiny();
        app.run_menu_action("select-mask");
        let ctx = crate::a11y_tests::ctx();
        for v in MaskView::ALL {
            app.select_mask.as_mut().unwrap().view = v;
            let missing = crate::a11y_tests::nameless(&mut app, &ctx);
            assert!(app.select_mask.is_some(), "still open");
            assert_eq!(missing, Vec::<String>::new(), "{v:?}");
        }
        // The overlay tints the unselected side red at the chosen opacity.
        let st = app.select_mask.as_mut().unwrap();
        st.view = MaskView::Overlay;
        st.opacity = 1.0;
        st.update();
        let img = st.image();
        let out = img.pixels[20 * 96 + 80];
        assert!(out.r() > 200 && out.g() < 60 && out.b() < 60, "{out:?}");
        st.view = MaskView::BlackWhite;
        let img = st.image();
        assert_eq!(img.pixels[20 * 96 + 10], Color32::WHITE);
        assert_eq!(img.pixels[20 * 96 + 80], Color32::BLACK);
    }
}
