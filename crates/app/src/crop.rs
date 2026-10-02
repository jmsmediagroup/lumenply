//! The Crop tool (C), Photoshop-style: picking it frames the whole canvas;
//! eight handles resize the frame, a drag inside moves it, a drag outside
//! turns it (straighten), and on a fresh frame a drag inside draws a new
//! one. Outside the frame is dimmed, thirds show while dragging, and a pill
//! reads out the size. The options bar holds ratio presets, custom W:H,
//! swap, "Delete cropped pixels", Cancel and Crop. Enter commits one
//! `CropCanvas` command (one undo step); Esc resets the frame.

use super::*;

/// Ratio presets in the options bar: Free, the canvas's own, and fixed W:H.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Preset {
    Free,
    Original,
    Ratio(f32, f32),
}

pub(crate) const PRESETS: [(&str, Preset); 6] = [
    ("Free", Preset::Free),
    ("Original", Preset::Original),
    ("1:1", Preset::Ratio(1.0, 1.0)),
    ("4:5", Preset::Ratio(4.0, 5.0)),
    ("3:2", Preset::Ratio(3.0, 2.0)),
    ("16:9", Preset::Ratio(16.0, 9.0)),
];

/// Screen-pixel reach of the corner and edge handles.
const CORNER_REACH: f32 = 10.0;
const EDGE_REACH: f32 = 7.0;

/// The crop frame: a rectangle around a centre, turned clockwise (on
/// screen) by `angle`, in document pixels. It may reach past the canvas.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Frame {
    pub(crate) cx: f32,
    pub(crate) cy: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    pub(crate) angle: f32,
    /// The canvas size the frame was made for; another size resets it.
    pub(crate) canvas: (u32, u32),
    /// Still the untouched whole-canvas frame: a drag inside draws a new one.
    pub(crate) pristine: bool,
}

impl Frame {
    pub(crate) fn whole(w: u32, h: u32) -> Frame {
        Frame {
            cx: w as f32 / 2.0,
            cy: h as f32 / 2.0,
            w: w as f32,
            h: h as f32,
            angle: 0.0,
            canvas: (w, h),
            pristine: true,
        }
    }

    /// Document point → frame-local (centre at the origin, unrotated).
    pub(crate) fn local(&self, p: (f32, f32)) -> (f32, f32) {
        let (s, c) = (-self.angle).sin_cos();
        let (x, y) = (p.0 - self.cx, p.1 - self.cy);
        (x * c - y * s, x * s + y * c)
    }

    /// Frame-local point → document.
    pub(crate) fn world(&self, q: (f32, f32)) -> (f32, f32) {
        let (s, c) = self.angle.sin_cos();
        (self.cx + q.0 * c - q.1 * s, self.cy + q.0 * s + q.1 * c)
    }

    /// Local corners: top-left, top-right, bottom-right, bottom-left.
    fn local_corners(&self) -> [(f32, f32); 4] {
        let (hw, hh) = (self.w / 2.0, self.h / 2.0);
        [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
    }

    pub(crate) fn corners(&self) -> [(f32, f32); 4] {
        self.local_corners().map(|q| self.world(q))
    }

    /// The whole-pixel crop rectangle (before rotation): edges round to
    /// the nearest pixel, never thinner than one.
    pub(crate) fn rect(&self) -> Rect {
        let x0 = (self.cx - self.w / 2.0).round() as i32;
        let y0 = (self.cy - self.h / 2.0).round() as i32;
        let x1 = ((self.cx + self.w / 2.0).round() as i32).max(x0 + 1);
        let y1 = ((self.cy + self.h / 2.0).round() as i32).max(y0 + 1);
        Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    /// The angle to commit: tiny turns count as none, so a plain crop
    /// stays an exact whole-pixel move.
    pub(crate) fn commit_angle(&self) -> f32 {
        if self.angle.abs() < 0.0005 {
            0.0
        } else {
            self.angle
        }
    }
}

/// What a press on the canvas grabbed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Grip {
    /// 0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left.
    Corner(usize),
    /// 0 top, 1 right, 2 bottom, 3 left.
    Edge(usize),
    Move,
    Rotate,
    New,
}

#[derive(Clone, Copy, Debug)]
struct CropDrag {
    grip: Grip,
    start: Frame,
    press: (f32, f32),
}

/// The Crop tool's state: the live frame and the options-bar settings.
pub(crate) struct CropTool {
    pub(crate) frame: Option<Frame>,
    /// Fixed width:height, or `None` for a free frame.
    pub(crate) ratio: Option<(f32, f32)>,
    /// The W and H fields as shown (0 = empty), which may be half-typed.
    pub(crate) fields: (f32, f32),
    pub(crate) delete_cropped: bool,
    drag: Option<CropDrag>,
    hover: Option<Grip>,
}

impl Default for CropTool {
    fn default() -> Self {
        CropTool {
            frame: None,
            ratio: None,
            fields: (0.0, 0.0),
            // Non-destructive by default: cropped-off pixels stay on their
            // layers until the user asks for them to go.
            delete_cropped: false,
            drag: None,
            hover: None,
        }
    }
}

/// Which grip a pointer at document point `p` is over, for a frame shown
/// at `zoom` (handle reach is measured in screen pixels).
pub(crate) fn grip_at(f: &Frame, p: (f32, f32), zoom: f32) -> Grip {
    let (qx, qy) = f.local(p);
    let (qx, qy) = (qx * zoom, qy * zoom);
    let (hw, hh) = (f.w / 2.0 * zoom, f.h / 2.0 * zoom);
    for (i, (cx, cy)) in [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
        .into_iter()
        .enumerate()
    {
        if (qx - cx).hypot(qy - cy) <= CORNER_REACH {
            return Grip::Corner(i);
        }
    }
    let along_x = qx.abs() <= hw;
    let along_y = qy.abs() <= hh;
    if along_x && (qy + hh).abs() <= EDGE_REACH {
        return Grip::Edge(0);
    }
    if along_y && (qx - hw).abs() <= EDGE_REACH {
        return Grip::Edge(1);
    }
    if along_x && (qy - hh).abs() <= EDGE_REACH {
        return Grip::Edge(2);
    }
    if along_y && (qx + hw).abs() <= EDGE_REACH {
        return Grip::Edge(3);
    }
    if along_x && along_y {
        if f.pristine {
            Grip::New
        } else {
            Grip::Move
        }
    } else {
        Grip::Rotate
    }
}

/// The local box spanned from anchor `a` towards `b`, grown to `ratio`
/// (W/H) when given: (x0, y0, x1, y1), normalised.
fn span(a: (f32, f32), b: (f32, f32), ratio: Option<f32>) -> (f32, f32, f32, f32) {
    let (mut w, mut h) = ((b.0 - a.0).abs().max(1.0), (b.1 - a.1).abs().max(1.0));
    if let Some(r) = ratio {
        if w / r >= h {
            h = w / r;
        } else {
            w = h * r;
        }
    }
    let x1 = if b.0 >= a.0 { a.0 + w } else { a.0 - w };
    let y1 = if b.1 >= a.1 { a.1 + h } else { a.1 - h };
    (a.0.min(x1), a.1.min(y1), a.0.max(x1), a.1.max(y1))
}

/// The frame after dragging `grip` from `press` to `to` (document
/// points), holding `ratio` (W/H) when one is set.
pub(crate) fn drag_frame(
    start: &Frame,
    grip: Grip,
    press: (f32, f32),
    to: (f32, f32),
    ratio: Option<f32>,
) -> Frame {
    let mut f = Frame {
        pristine: false,
        ..*start
    };
    let (a, b) = (start.local(press), start.local(to));
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (hw, hh) = (start.w / 2.0, start.h / 2.0);
    // Centre and size of a local box (in the start frame's axes).
    let place = |x0: f32, y0: f32, x1: f32, y1: f32| {
        let (cx, cy) = start.world(((x0 + x1) / 2.0, (y0 + y1) / 2.0));
        (cx, cy, (x1 - x0).max(1.0), (y1 - y0).max(1.0))
    };
    match grip {
        Grip::Move => {
            f.cx = start.cx + (to.0 - press.0);
            f.cy = start.cy + (to.1 - press.1);
        }
        Grip::Rotate => {
            let a0 = (press.1 - start.cy).atan2(press.0 - start.cx);
            let a1 = (to.1 - start.cy).atan2(to.0 - start.cx);
            let mut ang = start.angle + (a1 - a0);
            while ang > std::f32::consts::PI {
                ang -= std::f32::consts::TAU;
            }
            while ang < -std::f32::consts::PI {
                ang += std::f32::consts::TAU;
            }
            f.angle = ang;
            // A frame that sat inside the canvas shrinks (about its centre)
            // to stay inside while it turns, so straightening never adds
            // transparent corners. One reaching past the edges on purpose
            // keeps its size.
            let (cw, ch) = (start.canvas.0 as f32, start.canvas.1 as f32);
            let inside = |fr: &Frame| {
                fr.corners()
                    .iter()
                    .all(|&(x, y)| (-0.01..=cw + 0.01).contains(&x) && (-0.01..=ch + 0.01).contains(&y))
            };
            if inside(start) {
                let s = fit_scale(&f, cw, ch);
                f.w = (start.w * s).max(1.0);
                f.h = (start.h * s).max(1.0);
            }
        }
        Grip::New => {
            let (x0, y0, x1, y1) = span(a, b, ratio);
            (f.cx, f.cy, f.w, f.h) = place(x0, y0, x1, y1);
        }
        Grip::Corner(i) => {
            let corners = start.local_corners();
            let anchor = corners[(i + 2) % 4];
            let moved = (corners[i].0 + dx, corners[i].1 + dy);
            let (x0, y0, x1, y1) = span(anchor, moved, ratio);
            (f.cx, f.cy, f.w, f.h) = place(x0, y0, x1, y1);
        }
        Grip::Edge(i) => {
            let (mut x0, mut y0, mut x1, mut y1) = (-hw, -hh, hw, hh);
            match i {
                0 => y0 += dy,
                1 => x1 += dx,
                2 => y1 += dy,
                _ => x0 += dx,
            }
            let (x0n, x1n) = (x0.min(x1), x0.max(x1));
            let (y0n, y1n) = (y0.min(y1), y0.max(y1));
            let (mut x0, mut y0, mut x1, mut y1) = (x0n, y0n, x1n, y1n);
            if let Some(r) = ratio {
                // The other side follows, centred on its old middle.
                if i % 2 == 1 {
                    let h = (x1 - x0).max(1.0) / r;
                    (y0, y1) = (-h / 2.0, h / 2.0);
                } else {
                    let w = (y1 - y0).max(1.0) * r;
                    (x0, x1) = (-w / 2.0, w / 2.0);
                }
            }
            (f.cx, f.cy, f.w, f.h) = place(x0, y0, x1, y1);
        }
    }
    f
}

/// The largest scale (at most 1) of frame `f`, about its centre, whose
/// turned corners all lie inside a `cw` × `ch` canvas.
pub(crate) fn fit_scale(f: &Frame, cw: f32, ch: f32) -> f32 {
    let mut s: f32 = 1.0;
    let (sin, cos) = f.angle.sin_cos();
    for (hx, hy) in [(f.w / 2.0, f.h / 2.0), (f.w / 2.0, -f.h / 2.0)] {
        // Each corner and its mirror through the centre.
        let (ux, uy) = (hx * cos - hy * sin, hx * sin + hy * cos);
        for (u, c, size) in [(ux, f.cx, cw), (uy, f.cy, ch)] {
            if u.abs() > 1e-6 {
                // c ± s·|u| must stay within [0, size].
                s = s.min(c / u.abs()).min((size - c) / u.abs());
            }
        }
    }
    s.max(0.0)
}

/// The largest frame of `ratio` (W/H) that fits the canvas, centred as
/// near the frame's centre as the canvas allows. The turn is kept.
pub(crate) fn fit_ratio(f: &Frame, ratio: f32, canvas: (u32, u32)) -> Frame {
    let (cw, ch) = (canvas.0 as f32, canvas.1 as f32);
    let (w, h) = if cw / ch > ratio {
        (ch * ratio, ch)
    } else {
        (cw, cw / ratio)
    };
    Frame {
        cx: f.cx.clamp(w / 2.0, (cw - w / 2.0).max(w / 2.0)),
        cy: f.cy.clamp(h / 2.0, (ch - h / 2.0).max(h / 2.0)),
        w,
        h,
        pristine: false,
        ..*f
    }
}

/// `a:b` reduced by their greatest common divisor (1920:1080 → 16:9).
pub(crate) fn reduced(a: u32, b: u32) -> (f32, f32) {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let g = gcd(a, b).max(1);
    ((a / g) as f32, (b / g) as f32)
}

/// Where a ray from `c` at angle `ang` leaves the convex polygon `poly`
/// (which contains `c`).
fn ray_exit(poly: &[Pos2; 4], c: Pos2, ang: f32) -> Pos2 {
    let d = egui::vec2(ang.cos(), ang.sin());
    let mut best = f32::INFINITY;
    for k in 0..4 {
        let (p, q) = (poly[k], poly[(k + 1) % 4]);
        let e = q - p;
        let den = d.x * e.y - d.y * e.x;
        if den.abs() < 1e-9 {
            continue;
        }
        let w = p - c;
        let t = (w.x * e.y - w.y * e.x) / den;
        let s = (w.x * d.y - w.y * d.x) / den;
        if t > 0.0 && (-1e-4..=1.0 + 1e-4).contains(&s) && t < best {
            best = t;
        }
    }
    if best.is_finite() {
        c + d * best
    } else {
        c
    }
}

/// A mesh filling the ring between the convex quads `outer` and `inner`
/// (both around `c`): swept by angle, so it is exact for any turn.
fn ring_mesh(outer: &[Pos2; 4], inner: &[Pos2; 4], c: Pos2, color: Color32) -> egui::Mesh {
    let mut angles: Vec<f32> = outer
        .iter()
        .chain(inner.iter())
        .map(|p| (p.y - c.y).atan2(p.x - c.x))
        .collect();
    angles.sort_by(f32::total_cmp);
    angles.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    let mut mesh = egui::Mesh::default();
    let n = angles.len();
    for i in 0..n {
        let a0 = angles[i];
        let a1 = if i + 1 < n {
            angles[i + 1]
        } else {
            angles[0] + std::f32::consts::TAU
        };
        let base = mesh.vertices.len() as u32;
        for p in [
            ray_exit(outer, c, a0),
            ray_exit(outer, c, a1),
            ray_exit(inner, c, a1),
            ray_exit(inner, c, a0),
        ] {
            mesh.colored_vertex(p, color);
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    mesh
}

impl App {
    /// Keep the frame in step with the tool and the canvas: picking the
    /// Crop tool frames the whole canvas; leaving it drops the frame; a
    /// canvas that changed size underneath (undo, another tab) resets it.
    pub(crate) fn crop_sync(&mut self) {
        if self.tool != Tool::Crop || self.no_doc {
            self.crop.frame = None;
            self.crop.drag = None;
            return;
        }
        let doc = self.editor.doc();
        let (w, h) = (doc.width, doc.height);
        if self.crop.frame.is_none_or(|f| f.canvas != (w, h)) {
            // A selection made before picking the tool becomes the frame.
            let sel = doc
                .selection
                .as_ref()
                .map(|s| s.tight_bounds(doc.canvas()))
                .filter(|r| !r.is_empty());
            let mut f = Frame::whole(w, h);
            if let Some(r) = sel {
                f.cx = r.x as f32 + r.w as f32 / 2.0;
                f.cy = r.y as f32 + r.h as f32 / 2.0;
                f.w = r.w as f32;
                f.h = r.h as f32;
                f.pristine = false;
            }
            self.crop.frame = Some(f);
            self.crop.drag = None;
        }
    }

    /// Enter commits and Esc resets while the Crop tool is up (before Esc
    /// can drop the selection instead).
    pub(crate) fn crop_keys(&mut self, ctx: &egui::Context) {
        if self.tool != Tool::Crop || self.crop.frame.is_none() || ctx.wants_keyboard_input() {
            return;
        }
        let (enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        });
        if enter {
            self.commit_crop();
        } else if esc {
            self.cancel_crop();
        }
    }

    /// Crop to the frame: one undoable `CropCanvas`, then a fresh frame
    /// on the new canvas.
    pub(crate) fn commit_crop(&mut self) {
        let Some(f) = self.crop.frame else { return };
        let (rect, angle) = (f.rect(), f.commit_angle());
        if angle == 0.0 && rect == self.editor.doc().canvas() {
            self.status = "The frame covers the whole canvas: drag a handle first".into();
            return;
        }
        let before = self.editor.history().len();
        self.run(&CropCanvas {
            rect,
            angle,
            delete_cropped: self.crop.delete_cropped,
        });
        if self.editor.history().len() > before {
            self.crop.frame = None;
            self.crop.drag = None;
            self.crop_sync();
            self.view_cmd = Some(ViewCmd::Fit);
            self.status = format!("Cropped to {} × {} px", rect.w, rect.h);
        }
    }

    /// Put the frame back around the whole canvas.
    pub(crate) fn cancel_crop(&mut self) {
        let (w, h) = (self.editor.doc().width, self.editor.doc().height);
        self.crop.frame = Some(Frame::whole(w, h));
        self.crop.drag = None;
        self.status = "Crop reset".into();
    }

    /// Apply a new ratio (W:H, or `None` for free) and refit the frame.
    pub(crate) fn set_crop_ratio(&mut self, ratio: Option<(f32, f32)>) {
        self.crop.ratio = ratio.filter(|(a, b)| *a > 0.0 && *b > 0.0);
        self.crop.fields = self.crop.ratio.unwrap_or((0.0, 0.0));
        let canvas = (self.editor.doc().width, self.editor.doc().height);
        if let (Some((a, b)), Some(f)) = (self.crop.ratio, self.crop.frame.as_mut()) {
            *f = fit_ratio(f, a / b, canvas);
        }
    }

    /// The ratio as W/H, when fixed.
    fn crop_ratio(&self) -> Option<f32> {
        self.crop.ratio.map(|(a, b)| a / b)
    }

    /// Swap the ratio's sides (or, when free, the frame's).
    pub(crate) fn swap_crop_ratio(&mut self) {
        match self.crop.ratio {
            Some((a, b)) => self.set_crop_ratio(Some((b, a))),
            None => {
                if let Some(f) = self.crop.frame.as_mut() {
                    std::mem::swap(&mut f.w, &mut f.h);
                    f.pristine = false;
                }
            }
        }
    }

    /// The Crop tool on the canvas: hover cursors, grabbing, dragging.
    pub(crate) fn crop_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let Some(frame) = self.crop.frame else { return };
        let primary = egui::PointerButton::Primary;
        let zoom = self.zoom;
        self.crop.hover = resp.hover_pos().map(|p| grip_at(&frame, to_doc(p), zoom));
        let grip_now = self.crop.drag.map(|d| d.grip).or(self.crop.hover);
        if resp.hovered() || self.crop.drag.is_some() {
            if let Some(g) = grip_now {
                // Diagonals follow the frame's turn closely enough.
                let tilted = (frame.angle.to_degrees().rem_euclid(180.0) - 90.0).abs() < 45.0;
                ctx.set_cursor_icon(match g {
                    Grip::Corner(i) if (i % 2 == 0) != tilted => egui::CursorIcon::ResizeNwSe,
                    Grip::Corner(_) => egui::CursorIcon::ResizeNeSw,
                    Grip::Edge(i) if (i % 2 == 1) != tilted => egui::CursorIcon::ResizeHorizontal,
                    Grip::Edge(_) => egui::CursorIcon::ResizeVertical,
                    Grip::Move => egui::CursorIcon::Move,
                    Grip::Rotate => egui::CursorIcon::Alias,
                    Grip::New => egui::CursorIcon::Crosshair,
                });
            }
        }
        if resp.drag_started_by(primary) {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                let press = to_doc(p);
                self.crop.drag = Some(CropDrag {
                    grip: grip_at(&frame, press, zoom),
                    start: frame,
                    press,
                });
                self.begin_snap(&[]);
            }
        }
        if let (Some(d), true) = (self.crop.drag, resp.dragged_by(primary)) {
            if let Some(p) = resp.interact_pointer_pos() {
                let to = to_doc(p);
                // Shift holds the frame's own proportions when the ratio is
                // free (a new frame: a square).
                let shift = ctx.input(|i| i.modifiers.shift);
                let own = if d.grip == Grip::New {
                    1.0
                } else {
                    d.start.w / d.start.h.max(1.0)
                };
                let ratio = self.crop_ratio().or_else(|| shift.then_some(own));
                let mut f = drag_frame(&d.start, d.grip, d.press, to, ratio);
                // Snap what moves (axis-aligned frames only): the dragged
                // corner, the dragged edge, or the whole frame's edges and
                // centre; then redo the drag by the snapped amount.
                let (dx, dy) = if f.angle != 0.0 {
                    self.clear_snap_hint();
                    (0.0, 0.0)
                } else {
                    let (l, t, r, b) = (
                        f.cx - f.w / 2.0,
                        f.cy - f.h / 2.0,
                        f.cx + f.w / 2.0,
                        f.cy + f.h / 2.0,
                    );
                    match d.grip {
                        Grip::Corner(i) => {
                            let c = f.corners()[i];
                            let s = self.snap_point(c);
                            (s.0 - c.0, s.1 - c.1)
                        }
                        Grip::New => {
                            let s = self.snap_point(to);
                            (s.0 - to.0, s.1 - to.1)
                        }
                        Grip::Edge(i) => {
                            let (vertical, v) = match i {
                                0 => (false, t),
                                1 => (true, r),
                                2 => (false, b),
                                _ => (true, l),
                            };
                            let dv = self.snap_line(vertical, v) - v;
                            if vertical {
                                (dv, 0.0)
                            } else {
                                (0.0, dv)
                            }
                        }
                        Grip::Move => self.snap_rect_delta(l, t, r, b),
                        Grip::Rotate => (0.0, 0.0),
                    }
                };
                if dx != 0.0 || dy != 0.0 {
                    f = if d.grip == Grip::Move {
                        Frame {
                            cx: f.cx + dx,
                            cy: f.cy + dy,
                            ..f
                        }
                    } else {
                        drag_frame(&d.start, d.grip, d.press, (to.0 + dx, to.1 + dy), ratio)
                    };
                }
                self.crop.frame = Some(f);
            }
        }
        // Double-click inside the frame crops, as in Photoshop.
        if resp.double_clicked_by(primary) {
            let inside = resp
                .interact_pointer_pos()
                .is_some_and(|p| matches!(grip_at(&frame, to_doc(p), zoom), Grip::Move | Grip::New));
            if inside {
                self.commit_crop();
                return;
            }
        }
        if resp.drag_stopped() && self.crop.drag.is_some() {
            self.crop.drag = None;
            self.end_snap();
        }
    }

    /// The frame on the canvas: dimmed surround, checker where the crop
    /// adds canvas, border, thirds while dragging, handles, size pill.
    pub(crate) fn paint_crop(&self, ctx: &egui::Context, painter: &egui::Painter, resp: &egui::Response) {
        let Some(f) = self.crop.frame else { return };
        let origin = resp.rect.min + self.pan;
        let zoom = self.zoom;
        let ts = |p: (f32, f32)| egui::pos2(origin.x + p.0 * zoom, origin.y + p.1 * zoom);
        let quad: [Pos2; 4] = f.corners().map(ts);
        let centre = ts((f.cx, f.cy));
        let clip = painter.clip_rect();
        let bbox = egui::Rect::from_points(&quad);
        // Where an axis-aligned frame reaches past the canvas the crop adds
        // transparent canvas: show it as checkerboard.
        let doc = self.editor.doc();
        let doc_rect = egui::Rect::from_min_max(ts((0.0, 0.0)), ts((doc.width as f32, doc.height as f32)));
        if f.angle == 0.0 {
            let fr = bbox;
            let mid_y0 = fr.min.y.max(doc_rect.min.y);
            let mid_y1 = fr.max.y.min(doc_rect.max.y);
            let strips = [
                egui::Rect::from_min_max(fr.min, egui::pos2(fr.max.x, fr.max.y.min(doc_rect.min.y))),
                egui::Rect::from_min_max(egui::pos2(fr.min.x, fr.min.y.max(doc_rect.max.y)), fr.max),
                egui::Rect::from_min_max(
                    egui::pos2(fr.min.x, mid_y0),
                    egui::pos2(fr.max.x.min(doc_rect.min.x), mid_y1),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(fr.min.x.max(doc_rect.max.x), mid_y0),
                    egui::pos2(fr.max.x, mid_y1),
                ),
            ];
            for s in strips {
                if s.width() > 0.0 && s.height() > 0.0 {
                    paint_checker(painter, s, clip);
                }
            }
        }
        // Dim everything outside the frame.
        let outer_r = clip.union(bbox).expand(8.0);
        let outer = [
            outer_r.left_top(),
            outer_r.right_top(),
            outer_r.right_bottom(),
            outer_r.left_bottom(),
        ];
        painter.add(Shape::mesh(ring_mesh(
            &outer,
            &quad,
            centre,
            Color32::from_black_alpha(150),
        )));
        // Thirds while dragging.
        let dragging = self.crop.drag.is_some();
        if dragging {
            let thin = Stroke::new(1.0, Color32::from_white_alpha(110));
            let (hw, hh) = (f.w / 2.0, f.h / 2.0);
            for k in [1.0 / 3.0, 2.0 / 3.0] {
                let x = -hw + f.w * k;
                let y = -hh + f.h * k;
                painter.line_segment([ts(f.world((x, -hh))), ts(f.world((x, hh)))], thin);
                painter.line_segment([ts(f.world((-hw, y))), ts(f.world((hw, y)))], thin);
            }
        }
        // Border.
        let mut ring = quad.to_vec();
        ring.push(quad[0]);
        painter.add(Shape::line(
            ring.clone(),
            Stroke::new(2.0, Color32::from_black_alpha(140)),
        ));
        painter.add(Shape::line(ring, Stroke::new(1.0, Color32::WHITE)));
        // Handles: L brackets at the corners, bars at the edge middles.
        let active = self.crop.drag.map(|d| d.grip).or(self.crop.hover);
        let side = |a: Pos2, b: Pos2| (b - a).normalized();
        let len_x = (bbox.width().min(bbox.height()) / 3.0).clamp(4.0, 16.0);
        for i in 0..4 {
            let c = quad[i];
            let next = quad[(i + 1) % 4];
            let prev = quad[(i + 3) % 4];
            let ink = if active == Some(Grip::Corner(i)) {
                ACCENT
            } else {
                Color32::WHITE
            };
            let pts = vec![c + side(c, prev) * len_x, c, c + side(c, next) * len_x];
            painter.add(Shape::line(
                pts.clone(),
                Stroke::new(5.0, Color32::from_black_alpha(120)),
            ));
            painter.add(Shape::line(pts, Stroke::new(3.0, ink)));
        }
        for i in 0..4 {
            let (a, b) = (quad[i], quad[(i + 1) % 4]);
            let m = a + (b - a) * 0.5;
            let d = side(a, b) * (len_x * 0.6);
            let ink = if active == Some(Grip::Edge(i)) {
                ACCENT
            } else {
                Color32::WHITE
            };
            painter.line_segment([m - d, m + d], Stroke::new(5.0, Color32::from_black_alpha(120)));
            painter.line_segment([m - d, m + d], Stroke::new(3.0, ink));
        }
        // Live readout beside the pointer.
        if let (Some(d), Some(p)) = (self.crop.drag, ctx.input(|i| i.pointer.latest_pos())) {
            let r = f.rect();
            let text = if d.grip == Grip::Rotate {
                format!("{:+.1}°", f.angle.to_degrees())
            } else {
                format!("{} × {} px", r.w, r.h)
            };
            let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), TEXT);
            let at = p + egui::vec2(18.0, 18.0);
            let pill = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(8.0, 4.0));
            painter.rect_filled(pill, 6.0, RAISED);
            painter.rect_stroke(pill, 6.0, Stroke::new(1.0, LINE));
            painter.galley(at, galley, TEXT);
        }
    }

    /// The Crop tool's options bar: ratio, W:H, swap, delete cropped,
    /// size, Cancel and Crop.
    pub(crate) fn crop_options_bar(&mut self, ui: &mut egui::Ui, tier: options_bar::Tier) {
        let canvas = (self.editor.doc().width, self.editor.doc().height);
        let original = reduced(canvas.0, canvas.1);
        let label = match self.crop.ratio {
            None => "Free".to_string(),
            Some(r) if r == original => "Original".to_string(),
            Some((a, b)) => format!("{}:{}", trim(a), trim(b)),
        };
        ui.label(RichText::new("Ratio").color(MUTED));
        let mut pick = None;
        let r = egui::ComboBox::from_id_salt("crop-ratio")
            .selected_text(label)
            .width(88.0)
            .show_ui(ui, |ui| {
                popup_style(ui);
                for (name, p) in PRESETS {
                    if ui.selectable_label(false, name).clicked() {
                        pick = Some(p);
                    }
                }
            });
        a11y_name(&r.response, "Crop ratio");
        r.response
            .on_hover_text("Keep the frame at a fixed width : height");
        if let Some(p) = pick {
            let ratio = match p {
                Preset::Free => None,
                Preset::Original => Some(original),
                Preset::Ratio(a, b) => Some((a, b)),
            };
            self.set_crop_ratio(ratio);
        }
        // Custom W:H: typing both sides fixes the ratio; an empty side
        // leaves the frame free.
        let (mut a, mut b) = self.crop.fields;
        let field = |ui: &mut egui::Ui, v: &mut f32, name: &str| {
            let r = num_field(
                ui,
                egui::DragValue::new(v)
                    .range(0.0..=99_999.0)
                    .speed(0.05)
                    .max_decimals(2)
                    .custom_formatter(|v, _| if v <= 0.0 { String::new() } else { trim(v as f32) }),
                46.0,
            );
            a11y_name(&r, name);
            r.on_hover_text(format!("{name} (empty = free)")).changed()
        };
        let ca = field(ui, &mut a, "Ratio width");
        let swap = swap_button(ui).clicked();
        let cb = field(ui, &mut b, "Ratio height");
        if swap {
            self.swap_crop_ratio();
        } else if ca || cb {
            self.set_crop_ratio(Some((a, b)));
            // Keep a half-typed pair for the other field.
            self.crop.fields = (a, b);
        }
        ui.separator();
        check(ui, &mut self.crop.delete_cropped, "Delete cropped pixels").on_hover_text(
            "On: pixels outside the frame are removed. Off: layers keep them beyond the \
             canvas edge, so a later canvas enlargement brings them back",
        );
        if let Some(f) = self.crop.frame {
            ui.separator();
            let r = f.rect();
            let mut text = format!("{} × {} px", r.w, r.h);
            if f.commit_angle() != 0.0 {
                text.push_str(&format!("  {:+.1}°", f.angle.to_degrees()));
            }
            ui.label(RichText::new(text).monospace().color(TEXT))
                .on_hover_text("Size of the cropped canvas");
            if tier != options_bar::Tier::Tight {
                ui.label(
                    RichText::new("Drag outside the frame to straighten · Shift keeps proportions").weak(),
                );
            }
        }
        ui.separator();
        if ui
            .add(footer_button("Cancel").shortcut_text("Esc"))
            .on_hover_text("Reset the frame to the whole canvas (Esc)")
            .clicked()
        {
            self.cancel_crop();
        }
        if ui
            .add(
                primary_button("Crop")
                    .shortcut_text(RichText::new("Enter").color(ACCENT_INK.gamma_multiply(0.7))),
            )
            .on_hover_text("Crop to the frame (Enter)")
            .clicked()
        {
            self.commit_crop();
        }
    }
}

impl App {
    /// Debug tokens: `crop:frame=X0:Y0:X1:Y1` sets the frame (and picks
    /// the tool), `crop:angle=DEG` turns it, `crop:drag` shows it mid-drag
    /// (thirds, size pill), `crop:ratio=W:H` sets a ratio, `crop:commit`
    /// and `crop:cancel` press the buttons, `crop:delete` ticks "Delete
    /// cropped pixels".
    pub(crate) fn debug_crop(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("crop:") else {
            return false;
        };
        self.tool = Tool::Crop;
        self.crop_sync();
        let nums = |s: &str| -> Vec<f32> { s.split(':').filter_map(|v| v.parse().ok()).collect() };
        if let Some(v) = rest.strip_prefix("frame=") {
            if let (&[x0, y0, x1, y1], Some(f)) = (&nums(v)[..], self.crop.frame.as_mut()) {
                f.cx = (x0 + x1) / 2.0;
                f.cy = (y0 + y1) / 2.0;
                f.w = (x1 - x0).abs().max(1.0);
                f.h = (y1 - y0).abs().max(1.0);
                f.pristine = false;
            }
        } else if let Some(v) = rest.strip_prefix("angle=") {
            if let (Ok(deg), Some(f)) = (v.parse::<f32>(), self.crop.frame) {
                // Through a real rotate drag, so the frame fits as it turns.
                let r = f.w.max(f.h);
                let a = deg.to_radians();
                let to = (f.cx + r * a.cos(), f.cy + r * a.sin());
                let mut g = drag_frame(&f, Grip::Rotate, (f.cx + r, f.cy), to, None);
                g.angle = a;
                self.crop.frame = Some(g);
            }
        } else if let Some(v) = rest.strip_prefix("ratio=") {
            if let [a, b] = nums(v)[..] {
                self.set_crop_ratio(Some((a, b)));
            }
        } else if rest == "drag" {
            if let Some(f) = self.crop.frame {
                let corner = f.corners()[2];
                self.crop.drag = Some(CropDrag {
                    grip: Grip::Corner(2),
                    start: f,
                    press: corner,
                });
                // The size pill follows the pointer: add `popups:hover=X:Y`.
            }
        } else if rest == "commit" {
            self.commit_crop();
        } else if rest == "cancel" {
            self.cancel_crop();
        } else if rest == "delete" {
            self.crop.delete_cropped = true;
        } else if rest != "tool" {
            return false;
        }
        true
    }
}

/// A number without trailing zeros (16, 1.5).
fn trim(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The small swap-sides button between the ratio fields.
fn swap_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Swap width and height"));
    let ink = if resp.hovered() { TEXT } else { MUTED };
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, HOVER);
    }
    focus_ring(ui, &resp, rect, 4.0);
    let c = rect.center();
    let st = Stroke::new(1.3, ink);
    let p = ui.painter();
    p.line_segment([c + egui::vec2(-6.0, -2.5), c + egui::vec2(6.0, -2.5)], st);
    p.line_segment([c + egui::vec2(6.0, -2.5), c + egui::vec2(3.0, -5.5)], st);
    p.line_segment([c + egui::vec2(-6.0, 2.5), c + egui::vec2(6.0, 2.5)], st);
    p.line_segment([c + egui::vec2(-6.0, 2.5), c + egui::vec2(-3.0, 5.5)], st);
    resp.on_hover_text("Swap width and height")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x0: f32, y0: f32, x1: f32, y1: f32) -> Frame {
        Frame {
            cx: (x0 + x1) / 2.0,
            cy: (y0 + y1) / 2.0,
            w: x1 - x0,
            h: y1 - y0,
            angle: 0.0,
            canvas: (400, 300),
            pristine: false,
        }
    }

    fn edges(f: &Frame) -> (f32, f32, f32, f32) {
        (
            f.cx - f.w / 2.0,
            f.cy - f.h / 2.0,
            f.cx + f.w / 2.0,
            f.cy + f.h / 2.0,
        )
    }

    #[test]
    fn handles_are_found_by_screen_distance() {
        let f = frame(100.0, 100.0, 300.0, 200.0);
        // At 50% zoom 10 screen px are 20 document px.
        assert_eq!(grip_at(&f, (112.0, 112.0), 0.5), Grip::Corner(0));
        assert_eq!(grip_at(&f, (112.0, 112.0), 2.0), Grip::Move);
        assert_eq!(grip_at(&f, (300.0, 150.0), 1.0), Grip::Edge(1));
        assert_eq!(grip_at(&f, (200.0, 203.0), 1.0), Grip::Edge(2));
        assert_eq!(grip_at(&f, (350.0, 150.0), 1.0), Grip::Rotate);
        let fresh = Frame { pristine: true, ..f };
        assert_eq!(grip_at(&fresh, (200.0, 150.0), 1.0), Grip::New);
    }

    #[test]
    fn corners_resize_from_the_opposite_corner_and_keep_a_ratio() {
        let f = frame(100.0, 100.0, 300.0, 200.0);
        // Free: the bottom-right corner follows the pointer.
        let g = drag_frame(&f, Grip::Corner(2), (300.0, 200.0), (340.0, 260.0), None);
        assert_eq!(edges(&g), (100.0, 100.0, 340.0, 260.0));
        // 1:1 grows to the larger side.
        let g = drag_frame(&f, Grip::Corner(2), (300.0, 200.0), (340.0, 260.0), Some(1.0));
        assert_eq!(edges(&g), (100.0, 100.0, 340.0, 340.0));
        // Dragging the top-left past the anchor flips without inverting.
        let g = drag_frame(&f, Grip::Corner(0), (100.0, 100.0), (350.0, 250.0), None);
        assert_eq!(edges(&g), (300.0, 200.0, 350.0, 250.0));
        assert!(!g.pristine);
    }

    #[test]
    fn edges_move_one_side_and_a_ratio_centres_the_other() {
        let f = frame(100.0, 100.0, 300.0, 200.0);
        let g = drag_frame(&f, Grip::Edge(3), (100.0, 150.0), (60.0, 170.0), None);
        assert_eq!(edges(&g), (60.0, 100.0, 300.0, 200.0));
        // 2:1 from the right edge: 300 wide → 150 tall about y = 150.
        let g = drag_frame(&f, Grip::Edge(1), (300.0, 150.0), (400.0, 150.0), Some(2.0));
        assert_eq!(edges(&g), (100.0, 75.0, 400.0, 225.0));
    }

    #[test]
    fn moving_drawing_and_rotating() {
        let f = frame(100.0, 100.0, 300.0, 200.0);
        let g = drag_frame(&f, Grip::Move, (150.0, 150.0), (170.0, 140.0), None);
        assert_eq!(edges(&g), (120.0, 90.0, 320.0, 190.0));
        let g = drag_frame(&f, Grip::New, (50.0, 60.0), (10.0, 160.0), Some(0.5));
        assert_eq!(
            edges(&g),
            (0.0, 60.0, 50.0, 160.0),
            "drawn up-left from the press at 1:2"
        );
        // A quarter turn about the centre (200, 150).
        let g = drag_frame(&f, Grip::Rotate, (400.0, 150.0), (200.0, 350.0), None);
        assert!((g.angle - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert_eq!((g.cx, g.cy, g.w, g.h), (200.0, 150.0, 200.0, 100.0));
        // Resizing a turned frame works in its own axes: the right edge
        // of a frame turned 90° clockwise points down.
        let h = drag_frame(&g, Grip::Edge(1), (200.0, 250.0), (200.0, 270.0), None);
        assert!((h.w - 220.0).abs() < 1e-3 && (h.h - 100.0).abs() < 1e-3);
        assert!((h.cx - 200.0).abs() < 1e-3 && (h.cy - 160.0).abs() < 1e-3);
    }

    #[test]
    fn turning_a_frame_inside_the_canvas_shrinks_it_to_stay_inside() {
        let mut f = Frame::whole(200, 100);
        f.pristine = false;
        // 10° clockwise about (100, 50): the corner (100, 50) from the
        // centre turns to (89.80, 66.61), so the height limits the scale to
        // 50 / 66.61 = 0.7507.
        let a = 10f32.to_radians();
        let g = drag_frame(
            &f,
            Grip::Rotate,
            (300.0, 50.0),
            (100.0 + 200.0 * a.cos(), 50.0 + 200.0 * a.sin()),
            None,
        );
        assert!((g.angle - a).abs() < 1e-5);
        assert!(
            (g.w - 150.14).abs() < 0.02 && (g.h - 75.07).abs() < 0.02,
            "{} × {}",
            g.w,
            g.h
        );
        assert!(g
            .corners()
            .iter()
            .all(|&(x, y)| (-0.01..=200.01).contains(&x) && (-0.01..=100.01).contains(&y)));
        // Turning back to level restores nothing beyond the start: the
        // start frame of a new drag is the shrunk one.
        let h = drag_frame(&g, Grip::Rotate, (300.0, 50.0), (300.0, 50.0), None);
        assert!((h.w - g.w).abs() < 1e-3);
        // A frame reaching past the canvas keeps its size when turned.
        let big = Frame { w: 260.0, ..f };
        let k = drag_frame(
            &big,
            Grip::Rotate,
            (300.0, 50.0),
            (100.0 + 200.0 * a.cos(), 50.0 + 200.0 * a.sin()),
            None,
        );
        assert_eq!((k.w, k.h), (260.0, 100.0));
    }

    #[test]
    fn ratios_fit_the_canvas_and_round_to_whole_pixels() {
        let f = Frame::whole(1920, 1080);
        let g = fit_ratio(&f, 1.0, (1920, 1080));
        assert_eq!(g.rect(), Rect::new(420, 0, 1080, 1080));
        let g = fit_ratio(&frame(0.0, 0.0, 100.0, 100.0), 4.0 / 5.0, (1920, 1080));
        assert_eq!(
            g.rect(),
            Rect::new(0, 0, 864, 1080),
            "pushed back inside the canvas"
        );
        assert_eq!(reduced(1920, 1080), (16.0, 9.0));
        assert_eq!(reduced(1800, 1205), (360.0, 241.0));
        assert_eq!(frame(10.4, 20.6, 110.4, 70.7).rect(), Rect::new(10, 21, 100, 50));
        assert_eq!(trim(16.0), "16");
        assert_eq!(trim(1.5), "1.5");
    }

    /// The demo app with no dialog and no autosave (no frames run here,
    /// but keep the pattern of the a11y tests).
    fn demo_app() -> App {
        let mut app = App::launch(&["--demo".to_string()]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app
    }

    #[test]
    fn picking_the_tool_frames_the_canvas_and_enter_crops_once() {
        let mut app = demo_app();
        let (w, h) = (app.editor.doc().width, app.editor.doc().height);
        assert_eq!((w, h), (1800, 1205));
        app.tool = Tool::Crop;
        app.crop_sync();
        let f = app.crop.frame.expect("the tool frames the canvas");
        assert_eq!(f.rect(), Rect::new(0, 0, 1800, 1205));
        assert!(f.pristine);
        // Committing the untouched frame changes nothing.
        let steps = app.editor.history().len();
        app.commit_crop();
        assert_eq!(app.editor.history().len(), steps);

        app.crop.frame = Some(drag_frame(&f, Grip::Corner(0), (0.0, 0.0), (300.2, 205.4), None));
        app.commit_crop();
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (1500, 1000));
        assert_eq!(app.editor.history().last().copied(), Some("Crop"));
        assert_eq!(app.editor.history().len(), steps + 1, "one undo step");
        // Non-destructive by default: the photo's cropped-off pixels stay.
        let bg = app.editor.doc().layers()[0].pixels().unwrap();
        assert!(
            bg.get_pixel(-10, -10).a > 0.99,
            "pixels beyond the new edge are kept"
        );
        let fresh = app.crop.frame.expect("a fresh frame on the new canvas");
        assert_eq!(fresh.rect(), Rect::new(0, 0, 1500, 1000));
        // Leaving the tool drops the frame; Esc-style reset restores it.
        app.tool = Tool::Brush;
        app.crop_sync();
        assert!(app.crop.frame.is_none());
        app.run_menu_action("undo");
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (1800, 1205));
    }

    #[test]
    fn a_selection_becomes_the_first_frame() {
        let mut app = demo_app();
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(100, 50, 400, 300))),
        });
        app.tool = Tool::Crop;
        app.crop_sync();
        let f = app.crop.frame.unwrap();
        assert_eq!(f.rect(), Rect::new(100, 50, 400, 300));
        assert!(!f.pristine, "a drag inside moves it rather than drawing anew");
        // Punctuation shortcuts read as themselves in menus.
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {}); // fonts load on the first frame
        let semi = theme::shortcut_text(&ctx, egui::Modifiers::COMMAND, Key::Semicolon);
        let quote = theme::shortcut_text(&ctx, egui::Modifiers::COMMAND, Key::Quote);
        assert!(semi.ends_with(';') && !semi.contains("Semicolon"), "{semi}");
        assert!(quote.ends_with('\'') && !quote.contains("Quote"), "{quote}");
        assert!(theme::shortcut_text(&ctx, egui::Modifiers::COMMAND, Key::R).ends_with('R'));
    }

    #[test]
    fn ratio_presets_refit_the_frame_and_swap_flips_them() {
        let mut app = demo_app();
        app.tool = Tool::Crop;
        app.crop_sync();
        app.set_crop_ratio(Some((1.0, 1.0)));
        assert_eq!(app.crop.frame.unwrap().rect(), Rect::new(298, 0, 1205, 1205));
        app.set_crop_ratio(Some((16.0, 9.0)));
        app.swap_crop_ratio();
        assert_eq!(app.crop.ratio, Some((9.0, 16.0)));
        let r = app.crop.frame.unwrap().rect();
        assert_eq!((r.w, r.h), (678, 1205), "9:16 fills the height");
        // A half-typed custom ratio leaves the frame free.
        app.set_crop_ratio(Some((4.0, 0.0)));
        assert_eq!(app.crop.ratio, None);
    }

    #[test]
    fn the_dim_ring_covers_exactly_the_outside() {
        // Ring between a 100×100 square and a centred 40×40 square turned
        // 30°: its area is 10000 − 1600 whatever the turn.
        let outer = [
            egui::pos2(0.0, 0.0),
            egui::pos2(100.0, 0.0),
            egui::pos2(100.0, 100.0),
            egui::pos2(0.0, 100.0),
        ];
        let f = Frame {
            cx: 50.0,
            cy: 50.0,
            w: 40.0,
            h: 40.0,
            angle: 30f32.to_radians(),
            canvas: (100, 100),
            pristine: false,
        };
        let inner = f.corners().map(|(x, y)| egui::pos2(x, y));
        let mesh = ring_mesh(&outer, &inner, egui::pos2(50.0, 50.0), Color32::BLACK);
        let area: f32 = mesh
            .indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|k| mesh.vertices[t[k] as usize].pos);
                ((b - a).x * (c - a).y - (b - a).y * (c - a).x).abs() / 2.0
            })
            .sum();
        assert!((area - 8400.0).abs() < 0.5, "ring area {area}");
    }
}
