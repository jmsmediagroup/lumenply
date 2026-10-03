//! Crop ▸ Perspective: the Crop tool's second mode (the switch at the
//! start of its options bar), Photoshop's Perspective Crop. A drag draws a
//! rectangle; its four corners then drag freely onto the edges that should
//! come out straight (a document shot at an angle, a leaning façade), a
//! drag inside moves the whole quad. A perspective grid shows the mapping
//! live. Enter (or Crop, or a double-click inside) runs one
//! `PerspectiveCrop`: the quad becomes the whole canvas, at the size its
//! edges suggest or at the W × H typed in the bar. Text, shapes and smart
//! objects are rasterized by that step; the bar says so beforehand.

use super::*;
use lumenply_core::perspective_crop::PerspectiveCrop;
use lumenply_render::Homography;

/// Screen-pixel reach of a corner handle.
const CORNER_REACH: f32 = 11.0;
/// Grid cells along each side.
const GRID: u32 = 6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PGrip {
    /// 0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left.
    Corner(usize),
    Move,
    New,
}

#[derive(Clone, Copy, Debug)]
struct PDrag {
    grip: PGrip,
    start: Option<[(f32, f32); 4]>,
    press: (f32, f32),
}

/// The mode's state, kept on the Crop tool.
#[derive(Default)]
pub(crate) struct PerspCrop {
    /// Perspective mode is on.
    pub(crate) on: bool,
    /// Corners in canvas pixels: top-left, top-right, bottom-right,
    /// bottom-left.
    pub(crate) quad: Option<[(f32, f32); 4]>,
    /// The W × H fields (0 = from the quad's edges).
    pub(crate) size: (u32, u32),
    /// The canvas size the quad was drawn on; another size drops it.
    canvas: (u32, u32),
    drag: Option<PDrag>,
    hover: Option<PGrip>,
}

impl PerspCrop {
    /// The output size: the typed W × H, each side defaulting to the
    /// quad's average edge length.
    pub(crate) fn out_size(&self) -> Option<(u32, u32)> {
        let q = self.quad?;
        let (nw, nh) = PerspectiveCrop::natural_size(q);
        let pick = |typed: u32, natural: u32| if typed > 0 { typed } else { natural };
        Some((pick(self.size.0, nw), pick(self.size.1, nh)))
    }
}

/// An axis-aligned quad spanning two points.
fn rect_quad(a: (f32, f32), b: (f32, f32)) -> [(f32, f32); 4] {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
    [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// Whether `p` lies inside the convex quad (either winding).
fn inside(q: &[(f32, f32); 4], p: (f32, f32)) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if c != 0.0 {
            if sign != 0.0 && c.signum() != sign {
                return false;
            }
            sign = c.signum();
        }
    }
    true
}

/// The grip under document point `p` (corners by screen distance).
pub(crate) fn pgrip_at(quad: Option<[(f32, f32); 4]>, p: (f32, f32), zoom: f32) -> PGrip {
    let Some(q) = quad else { return PGrip::New };
    let reach = CORNER_REACH / zoom.max(1e-3);
    let near = q
        .iter()
        .enumerate()
        .map(|(i, c)| (i, (c.0 - p.0).hypot(c.1 - p.1)))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    match near {
        Some((i, _)) => PGrip::Corner(i),
        None if inside(&q, p) => PGrip::Move,
        None => PGrip::New,
    }
}

impl App {
    /// Enter crops, Esc clears the quad.
    pub(crate) fn pcrop_keys(&mut self, ctx: &egui::Context) {
        if self.tool != Tool::Crop || ctx.wants_keyboard_input() {
            return;
        }
        let (enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                self.crop.persp.quad.is_some() && i.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        });
        if enter {
            self.commit_pcrop();
        } else if esc {
            self.cancel_pcrop();
        }
    }

    /// Runs the crop: one `PerspectiveCrop`, then a fit view.
    pub(crate) fn commit_pcrop(&mut self) {
        let (Some(quad), Some((w, h))) = (self.crop.persp.quad, self.crop.persp.out_size()) else {
            self.status = "Drag a frame over the image first".into();
            return;
        };
        if !PerspectiveCrop::is_convex(quad) {
            self.status = "The corners must make a convex shape".into();
            return;
        }
        let raster = PerspectiveCrop::rasterized_layers(self.editor.doc()).len();
        let before = self.editor.revision();
        self.run(&PerspectiveCrop {
            quad,
            width: w,
            height: h,
        });
        if self.editor.revision() != before {
            self.crop.persp.quad = None;
            self.crop.persp.drag = None;
            self.crop_sync();
            self.view_cmd = Some(ViewCmd::Fit);
            self.status = match raster {
                0 => format!("Perspective crop to {w} × {h} px"),
                1 => format!("Perspective crop to {w} × {h} px (1 layer rasterized)"),
                n => format!("Perspective crop to {w} × {h} px ({n} layers rasterized)"),
            };
        }
    }

    pub(crate) fn cancel_pcrop(&mut self) {
        self.crop.persp.quad = None;
        self.crop.persp.drag = None;
        self.status = "Perspective crop cleared".into();
    }

    /// Pointer input on the canvas in perspective mode.
    pub(crate) fn pcrop_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        // The canvas changed under the frame (undo, Image Size): start over.
        let canvas = (self.editor.doc().width, self.editor.doc().height);
        if self.crop.persp.canvas != canvas {
            self.crop.persp.canvas = canvas;
            self.crop.persp.quad = None;
            self.crop.persp.drag = None;
        }
        let zoom = self.zoom;
        let st = &mut self.crop.persp;
        st.hover = resp.hover_pos().map(|p| pgrip_at(st.quad, to_doc(p), zoom));
        if resp.hovered() || st.drag.is_some() {
            if let Some(g) = st.drag.map(|d| d.grip).or(st.hover) {
                ctx.set_cursor_icon(match g {
                    PGrip::Corner(_) => egui::CursorIcon::Crosshair,
                    PGrip::Move => egui::CursorIcon::Move,
                    PGrip::New => egui::CursorIcon::Cell,
                });
            }
        }
        if resp.drag_started_by(primary) {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                let press = to_doc(p);
                st.drag = Some(PDrag {
                    grip: pgrip_at(st.quad, press, zoom),
                    start: st.quad,
                    press,
                });
            }
        }
        if let (Some(d), true) = (st.drag, resp.dragged_by(primary)) {
            if let Some(p) = resp.interact_pointer_pos() {
                let to = to_doc(p);
                let (dx, dy) = (to.0 - d.press.0, to.1 - d.press.1);
                st.quad = match (d.grip, d.start) {
                    (PGrip::Corner(i), Some(mut q)) => {
                        q[i] = (q[i].0 + dx, q[i].1 + dy);
                        Some(q)
                    }
                    (PGrip::Move, Some(q)) => Some(q.map(|c| (c.0 + dx, c.1 + dy))),
                    _ if dx.abs() >= 1.0 && dy.abs() >= 1.0 => Some(rect_quad(d.press, to)),
                    _ => st.quad,
                };
            }
        }
        if resp.double_clicked_by(primary) {
            let hit = resp
                .interact_pointer_pos()
                .is_some_and(|p| matches!(pgrip_at(st.quad, to_doc(p), zoom), PGrip::Move));
            if hit {
                st.drag = None;
                self.commit_pcrop();
                return;
            }
        }
        if resp.drag_stopped() {
            st.drag = None;
        }
    }

    /// The quad, its perspective grid, the dimmed surround and handles.
    pub(crate) fn paint_pcrop(&self, ctx: &egui::Context, painter: &egui::Painter, resp: &egui::Response) {
        let st = &self.crop.persp;
        let origin = resp.rect.min + self.pan;
        let zoom = self.zoom;
        let ts = |p: (f32, f32)| egui::pos2(origin.x + p.0 * zoom, origin.y + p.1 * zoom);
        let Some(q) = st.quad else {
            // A hint at the top of the view until the first frame is drawn.
            let at = egui::pos2(resp.rect.center().x, resp.rect.top() + 28.0);
            let galley = painter.layout_no_wrap(
                "Drag a frame, then move its corners onto edges that should be straight".into(),
                FontId::proportional(13.0),
                TEXT,
            );
            let r = egui::Rect::from_center_size(at, galley.size()).expand2(egui::vec2(10.0, 6.0));
            painter.rect_filled(r, 6.0, RAISED.gamma_multiply(0.92));
            painter.galley(r.min + egui::vec2(10.0, 6.0), galley, TEXT);
            return;
        };
        let quad: [Pos2; 4] = q.map(ts);
        let convex = PerspectiveCrop::is_convex(q);
        let centre = Pos2::new(
            quad.iter().map(|p| p.x).sum::<f32>() / 4.0,
            quad.iter().map(|p| p.y).sum::<f32>() / 4.0,
        );
        let clip = painter.clip_rect();
        if convex {
            let outer_r = clip.union(egui::Rect::from_points(&quad)).expand(8.0);
            let outer = [
                outer_r.left_top(),
                outer_r.right_top(),
                outer_r.right_bottom(),
                outer_r.left_bottom(),
            ];
            painter.add(Shape::mesh(crop::ring_mesh(
                &outer,
                &quad,
                centre,
                Color32::from_black_alpha(140),
            )));
            // The grid: lines of the rectified rectangle, through the
            // homography, so they converge as the result will straighten.
            if let Some(h) = Homography::rect_to_quad(Rect::new(0, 0, GRID, GRID), q) {
                let thin = Stroke::new(1.0, Color32::from_white_alpha(90));
                let g = GRID as f32;
                for k in 1..GRID {
                    let k = k as f32;
                    for (a, b) in [((k, 0.0), (k, g)), ((0.0, k), (g, k))] {
                        if let (Some(pa), Some(pb)) = (h.apply(a.0, a.1), h.apply(b.0, b.1)) {
                            painter.line_segment([ts(pa), ts(pb)], thin);
                        }
                    }
                }
            }
        }
        let ink = if convex { Color32::WHITE } else { DANGER };
        let mut ring = quad.to_vec();
        ring.push(quad[0]);
        painter.add(Shape::line(
            ring.clone(),
            Stroke::new(2.0, Color32::from_black_alpha(140)),
        ));
        painter.add(Shape::line(ring, Stroke::new(1.0, ink)));
        let active = st.drag.map(|d| d.grip).or(st.hover);
        for (i, c) in quad.iter().enumerate() {
            let hot = active == Some(PGrip::Corner(i));
            let r = egui::Rect::from_center_size(*c, egui::vec2(9.0, 9.0));
            painter.rect_filled(r.expand(1.5), 1.0, Color32::from_black_alpha(170));
            painter.rect_filled(r, 1.0, if hot { ACCENT } else { Color32::WHITE });
        }
        if let (Some(_), Some(p), Some((w, h))) =
            (st.drag, ctx.input(|i| i.pointer.latest_pos()), st.out_size())
        {
            let galley = painter.layout_no_wrap(format!("{w} × {h} px"), FontId::monospace(12.0), TEXT);
            let at = p + egui::vec2(18.0, 18.0);
            let pill = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(8.0, 4.0));
            painter.rect_filled(pill, 6.0, RAISED);
            painter.rect_stroke(pill, 6.0, Stroke::new(1.0, LINE));
            painter.galley(at, galley, TEXT);
        }
    }

    /// The Crop / Perspective switch at the start of the Crop tool's bar;
    /// true when perspective mode drew the rest of the bar.
    pub(crate) fn pcrop_mode_switch(&mut self, ui: &mut egui::Ui, tier: options_bar::Tier) -> bool {
        let mut on = self.crop.persp.on;
        let persp = if tier == options_bar::Tier::Tight {
            "Persp."
        } else {
            "Perspective"
        };
        segmented(ui, &mut on, &[(false, "Crop"), (true, persp)]);
        if on != self.crop.persp.on {
            self.crop.persp.on = on;
            self.crop.persp.drag = None;
        }
        ui.separator();
        if !on {
            return false;
        }
        self.pcrop_options_bar(ui, tier);
        true
    }

    fn pcrop_options_bar(&mut self, ui: &mut egui::Ui, tier: options_bar::Tier) {
        let natural = self.crop.persp.quad.map(PerspectiveCrop::natural_size);
        let (mut w, mut h) = self.crop.persp.size;
        let field = |ui: &mut egui::Ui, v: &mut u32, name: &str, auto: Option<u32>| {
            let r = num_field(
                ui,
                egui::DragValue::new(v)
                    .range(0..=lumenply_core::crop::MAX_CROP_SIDE)
                    .speed(1.0)
                    .custom_formatter(move |v, _| {
                        if v <= 0.0 {
                            auto.map_or(String::new(), |a| format!("{a}"))
                        } else {
                            format!("{}", v as u32)
                        }
                    }),
                58.0,
            );
            a11y_name(&r, name);
            r.on_hover_text(format!("{name} of the result in px (0 = from the frame's edges)"))
                .changed()
        };
        ui.label(RichText::new("W").color(MUTED));
        let cw = field(ui, &mut w, "Width", natural.map(|n| n.0));
        ui.label(RichText::new("H").color(MUTED));
        let ch = field(ui, &mut h, "Height", natural.map(|n| n.1));
        if cw || ch {
            self.crop.persp.size = (w, h);
        }
        if self.crop.persp.size != (0, 0)
            && ui
                .small_button("Auto")
                .on_hover_text("Size the result from the frame's edges")
                .clicked()
        {
            self.crop.persp.size = (0, 0);
        }
        let tight = tier == options_bar::Tier::Tight;
        if let (Some((ow, oh)), false) = (self.crop.persp.out_size(), tight) {
            ui.separator();
            ui.label(RichText::new(format!("{ow} × {oh} px")).monospace().color(TEXT))
                .on_hover_text("Size of the rectified canvas");
        }
        let raster = PerspectiveCrop::rasterized_layers(self.editor.doc()).len();
        if raster > 0 {
            let layers = if raster == 1 { "layer" } else { "layers" };
            let text = if tier == options_bar::Tier::Wide {
                format!("rasterizes {raster} text/vector {layers}")
            } else {
                format!("rasterizes {raster} {layers}")
            };
            ui.label(RichText::new(text).color(ACCENT)).on_hover_text(format!(
                "{raster} text, shape or smart-object layer(s) cannot follow a perspective \
                 change; the crop turns them into pixels (one undo step)"
            ));
        } else if tier == options_bar::Tier::Wide && self.crop.persp.quad.is_some() {
            ui.label(RichText::new("Drag corners onto edges that should be straight").weak());
        }
        ui.separator();
        if ui
            .add(footer_button("Cancel").shortcut_text("Esc"))
            .on_hover_text("Clear the frame (Esc)")
            .clicked()
        {
            self.cancel_pcrop();
        }
        let ready = self.crop.persp.quad.is_some();
        if ui
            .add_enabled(
                ready,
                primary_button("Crop")
                    .shortcut_text(RichText::new("Enter").color(ACCENT_INK.gamma_multiply(0.7))),
            )
            .on_hover_text("Rectify and crop to the frame (Enter)")
            .clicked()
        {
            self.commit_pcrop();
        }
    }

    /// The palette's "Perspective Crop tool": the Crop tool in
    /// perspective mode.
    pub(crate) fn pick_perspective_crop(&mut self) {
        self.tool = Tool::Crop;
        self.crop.persp.on = true;
        self.crop_sync();
    }

    /// Debug tokens: `pcrop:on` picks the Crop tool in perspective mode;
    /// `pcrop:quad=X0:Y0:X1:Y1:X2:Y2:X3:Y3` sets the corners (tl, tr, br,
    /// bl, canvas px); `pcrop:size=W:H`; `pcrop:drag` shows it mid-drag;
    /// `pcrop:commit`, `pcrop:cancel`; `pcrop:off` back to plain crop.
    pub(crate) fn debug_pcrop(&mut self, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("pcrop:") else {
            return false;
        };
        self.tool = Tool::Crop;
        self.crop_sync();
        self.crop.persp.on = rest != "off";
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let n: Vec<f32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        match (verb, n.as_slice()) {
            ("on" | "off", _) => {}
            ("quad", &[x0, y0, x1, y1, x2, y2, x3, y3]) => {
                self.crop.persp.quad = Some([(x0, y0), (x1, y1), (x2, y2), (x3, y3)]);
                self.crop.persp.canvas = (self.editor.doc().width, self.editor.doc().height);
            }
            ("size", &[w, h]) => self.crop.persp.size = (w as u32, h as u32),
            ("drag", _) => {
                if let Some(q) = self.crop.persp.quad {
                    self.crop.persp.drag = Some(PDrag {
                        grip: PGrip::Corner(1),
                        start: Some(q),
                        press: q[1],
                    });
                }
            }
            ("commit", _) => self.commit_pcrop(),
            ("cancel", _) => self.cancel_pcrop(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::{Rgba, TileStore};

    fn launch_doc() -> App {
        let mut doc = Document::new(200, 120);
        let id = doc.add_pixel_layer("Photo");
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() =
            TileStore::from_raster(&Raster::filled(200, 120, Rgba::new(0.5, 0.2, 0.1, 1.0)), 0, 0);
        let mut app = crate::a11y_tests::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app
    }

    #[test]
    fn grips_find_corners_by_screen_distance_and_the_inside() {
        let q = Some([(10.0, 10.0), (110.0, 20.0), (100.0, 90.0), (20.0, 80.0)]);
        assert_eq!(pgrip_at(q, (14.0, 13.0), 1.0), PGrip::Corner(0));
        assert_eq!(
            pgrip_at(q, (14.0, 13.0), 0.25),
            PGrip::Corner(0),
            "zoomed out reaches further"
        );
        assert_eq!(
            pgrip_at(q, (30.0, 30.0), 4.0),
            PGrip::Move,
            "zoomed in: inside, not a corner"
        );
        assert_eq!(pgrip_at(q, (60.0, 50.0), 1.0), PGrip::Move);
        assert_eq!(pgrip_at(q, (150.0, 50.0), 1.0), PGrip::New);
        assert_eq!(pgrip_at(None, (60.0, 50.0), 1.0), PGrip::New);
        assert_eq!(
            rect_quad((50.0, 40.0), (10.0, 60.0)),
            [(10.0, 40.0), (50.0, 40.0), (50.0, 60.0), (10.0, 60.0)]
        );
    }

    #[test]
    fn enter_crops_to_the_rectified_quad_in_one_step() {
        let mut app = launch_doc();
        let steps = app.editor.history().len();
        assert!(app.debug_pcrop("pcrop:on"));
        assert_eq!(app.tool, Tool::Crop);
        app.debug_pcrop("pcrop:commit");
        assert_eq!(app.editor.history().len(), steps, "no frame yet");
        // A trapezoid: top 100 wide, bottom 140, sides 53.9 long.
        app.debug_pcrop("pcrop:quad=50:30:150:30:170:80:30:80");
        assert_eq!(app.crop.persp.out_size(), Some((120, 54)));
        app.debug_pcrop("pcrop:size=90:60");
        assert_eq!(app.crop.persp.out_size(), Some((90, 60)));
        app.debug_pcrop("pcrop:commit");
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last(), Some(&"Perspective Crop"));
        let d = app.editor.doc();
        assert_eq!((d.width, d.height), (90, 60));
        let p = d.layers()[0].pixels().unwrap().get_pixel(45, 30);
        assert!((p.r - 0.5).abs() < 1e-3 && (p.a - 1.0).abs() < 1e-3, "{p:?}");
        assert!(app.crop.persp.quad.is_none(), "the frame is used up");
    }

    #[test]
    fn a_frame_on_another_canvas_size_is_dropped() {
        let mut app = launch_doc();
        app.debug_pcrop("pcrop:quad=50:30:150:30:170:80:30:80");
        app.crop.persp.canvas = (10, 10);
        let ctx = crate::a11y_tests::ctx();
        crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(app.crop.persp.quad.is_none());
    }

    #[test]
    fn a_crossed_quad_is_refused_and_switching_modes_keeps_the_plain_crop() {
        let mut app = launch_doc();
        let steps = app.editor.history().len();
        app.debug_pcrop("pcrop:quad=10:10:100:100:100:10:10:100");
        app.commit_pcrop();
        assert_eq!(app.editor.history().len(), steps);
        assert_eq!(app.status, "The corners must make a convex shape");
        app.debug_pcrop("pcrop:off");
        assert!(!app.crop.persp.on);
        assert!(app.crop.frame.is_some(), "the plain crop frame is back");
    }

    #[test]
    fn the_bar_and_canvas_name_every_control() {
        let mut app = launch_doc();
        app.debug_pcrop("pcrop:quad=50:30:150:30:170:80:30:80");
        let ctx = crate::a11y_tests::ctx();
        assert_eq!(crate::a11y_tests::nameless(&mut app, &ctx), Vec::<String>::new());
    }
}
