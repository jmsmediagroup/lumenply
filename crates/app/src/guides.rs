//! View aids on the canvas: rulers (View ▸ Rulers), guides (drag out of a
//! ruler; move with the Move tool; drag back onto the ruler to delete),
//! the grid, and snapping for the Move tool, marquees, the crop frame and
//! free transform. The guides themselves live in the document (undoable
//! commands in `lumenply_core::guides`); the on/off switches live in the
//! preferences.

use super::*;
use lumenply_core::snap::{self, SnapLines, SnapOptions, SnapSource};
use lumenply_doc::Guide;

/// Ruler thickness in points.
pub(crate) const RULER: f32 = 18.0;
/// How near (screen pixels) an edge must come to a line to snap.
pub(crate) const SNAP_PX: f32 = 6.0;
/// How near (screen pixels) the pointer must be to grab a guide.
const GUIDE_REACH: f32 = 4.0;
/// Guides are cyan, as everywhere; the snap hint wears the accent.
pub(crate) const GUIDE_INK: Color32 = Color32::from_rgb(0x4D, 0xC8, 0xF0);

/// The View-menu actions this module runs (see palette.rs).
pub(crate) const VIEW_ACTIONS: [&str; 8] = [
    "rulers",
    "guides",
    "lock-guides",
    "clear-guides",
    "new-guide",
    "grid",
    "snap",
    "smart-guides",
];

/// Transient state of the view aids (the switches are in `Prefs`).
#[derive(Default)]
pub(crate) struct ViewAids {
    /// Lines collected when a snapping drag began; `None` while idle or
    /// with snapping off.
    lines: Option<SnapLines>,
    /// The vertical (x) and horizontal (y) lines the drag snapped to,
    /// shown as accent lines.
    hint: [Option<f32>; 2],
    /// A guide being dragged: out of a ruler (`index` None) or an existing one.
    guide_drag: Option<GuideDrag>,
    /// Painted bounds of the layer the Move tool is dragging.
    pub(crate) move_bounds: Option<Rect>,
    /// The marquee's snapped corners (document space) while dragging.
    marquee: Option<((f32, f32), (f32, f32))>,
    /// The canvas area on screen last frame (tests and debug tokens).
    pub(crate) view: Option<egui::Rect>,
    /// Smart Guides for the Move drag under way (smart_guides.rs).
    pub(crate) smart: crate::smart_guides::SmartGuides,
}

#[derive(Clone, Copy, Debug)]
struct GuideDrag {
    index: Option<usize>,
    vertical: bool,
    pos: f32,
}

/// Ruler tick spacing for a zoom: the major step in document pixels (1, 2
/// or 5 × 10ⁿ, at least 64 screen pixels apart) and minor ticks per major.
pub(crate) fn ruler_steps(zoom: f32) -> (f32, u32) {
    let zoom = zoom.max(1e-4);
    let mut step = 1.0;
    'search: for exp in -2..8 {
        for m in [1.0, 2.0, 5.0] {
            step = m * 10f32.powi(exp);
            if step * zoom >= 64.0 {
                break 'search;
            }
        }
    }
    let minor = [10u32, 5, 2]
        .into_iter()
        .find(|n| step / *n as f32 * zoom >= 6.0)
        .unwrap_or(1);
    (step, minor)
}

/// A ruler label: whole numbers without decimals.
fn ruler_label(v: f32, step: f32) -> String {
    if step >= 1.0 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

impl App {
    // ---- snapping ------------------------------------------------------------------

    /// Start a snapping drag: collect the lines once (layer bounds cost a
    /// pixel scan). `skip` are the layers being moved.
    pub(crate) fn begin_snap(&mut self, skip: &[LayerId]) {
        self.aids.hint = [None, None];
        self.aids.lines = self.prefs.snap.then(|| {
            let opts = SnapOptions {
                guides: self.prefs.show_guides,
                canvas: true,
                layers: true,
                grid: self.prefs.show_grid.then(|| self.grid_step()),
            };
            snap::doc_lines(self.editor.doc(), &opts, skip)
        });
    }

    pub(crate) fn end_snap(&mut self) {
        self.aids.lines = None;
        self.aids.hint = [None, None];
        self.aids.move_bounds = None;
        self.aids.marquee = None;
        self.end_smart_guides();
    }

    pub(crate) fn clear_snap_hint(&mut self) {
        self.aids.hint = [None, None];
    }

    /// Which axes (x, y) the last snap landed on a line.
    pub(crate) fn snap_held(&self) -> [bool; 2] {
        [self.aids.hint[0].is_some(), self.aids.hint[1].is_some()]
    }

    fn snap_tol(&self) -> f32 {
        SNAP_PX / self.zoom.max(1e-4)
    }

    /// The grid's finest line spacing in document pixels.
    pub(crate) fn grid_step(&self) -> f32 {
        self.prefs.grid_spacing.max(1.0) / self.prefs.grid_subdivisions.max(1) as f32
    }

    /// Snap a dragged point (document space); remembers what it hit.
    pub(crate) fn snap_point(&mut self, p: (f32, f32)) -> (f32, f32) {
        let Some(lines) = &self.aids.lines else { return p };
        let (q, [sx, sy]) = lines.snap_point(p.0, p.1, self.snap_tol());
        self.aids.hint = [sx.map(|s| s.line), sy.map(|s| s.line)];
        q
    }

    /// The nudge that snaps a moving rectangle by its edges or centre.
    pub(crate) fn snap_rect_delta(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> (f32, f32) {
        let Some(lines) = &self.aids.lines else {
            return (0.0, 0.0);
        };
        let [sx, sy] = lines.snap_rect(x0, y0, x1, y1, self.snap_tol());
        self.aids.hint = [sx.map(|s| s.line), sy.map(|s| s.line)];
        (sx.map_or(0.0, |s| s.delta), sy.map_or(0.0, |s| s.delta))
    }

    /// Move tool: a drag on the active layer starts; snap its bounds.
    pub(crate) fn begin_move_snap(&mut self) {
        let skip: Vec<LayerId> = self.active.into_iter().collect();
        self.begin_snap(&skip);
        self.aids.move_bounds = self
            .active_layer()
            .and_then(|l| l.raster_store())
            .and_then(snap::painted_bounds);
        self.begin_smart_guides(&skip);
    }

    /// Move tool: the whole-pixel offset after snapping the moved bounds
    /// (View ▸ Snap first, then Smart Guides on the axes it left free).
    /// `free` (Cmd held) turns both off for this frame.
    pub(crate) fn snap_move_offset(&mut self, off: (i32, i32), free: bool) -> (i32, i32) {
        let Some(b) = self.aids.move_bounds else {
            return off;
        };
        let off = if free {
            self.clear_snap_hint();
            off
        } else {
            let (x0, y0) = ((b.x + off.0) as f32, (b.y + off.1) as f32);
            let (dx, dy) = self.snap_rect_delta(x0, y0, x0 + b.w as f32, y0 + b.h as f32);
            (off.0 + dx.round() as i32, off.1 + dy.round() as i32)
        };
        let held = self.snap_held();
        self.smart_move_offset(off, held, free)
    }

    /// Marquee: snap both corners of the drag (document space) and keep
    /// them for the outline and the release.
    pub(crate) fn track_marquee(&mut self, a: (f32, f32), b: (f32, f32)) {
        let a = self.snap_point(a);
        let hint_a = self.aids.hint;
        let b = self.snap_point(b);
        // Show whichever corner snapped on each axis.
        self.aids.hint = [self.aids.hint[0].or(hint_a[0]), self.aids.hint[1].or(hint_a[1])];
        self.aids.marquee = Some((a, b));
    }

    /// The marquee's snapped corners, if a drag tracked them.
    pub(crate) fn marquee_doc(&self) -> Option<((f32, f32), (f32, f32))> {
        self.aids.marquee
    }

    // ---- painting --------------------------------------------------------------------

    /// Grid (over the image) and guides (across the whole view).
    pub(crate) fn paint_view_aids(&self, painter: &egui::Painter, clip: egui::Rect, doc_rect: egui::Rect) {
        let zoom = self.zoom;
        let origin = doc_rect.min;
        if self.prefs.show_grid {
            let area = doc_rect.intersect(clip);
            let major = self.prefs.grid_spacing.max(1.0);
            let subs = self.prefs.grid_subdivisions.max(1);
            let minor = major / subs as f32;
            // Faint subdivisions only when they are far enough apart to read.
            let step = if minor * zoom >= 6.0 { minor } else { major };
            if step * zoom >= 4.0 && area.width() > 0.0 && area.height() > 0.0 {
                let line = |k: i64| {
                    let major_line = (k % subs as i64) == 0 || step == major;
                    if major_line {
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(150, 160, 175, 96))
                    } else {
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(150, 160, 175, 34))
                    }
                };
                let k0 = ((area.min.x - origin.x) / zoom / step).ceil() as i64;
                let k1 = ((area.max.x - origin.x) / zoom / step).floor() as i64;
                for k in k0..=k1 {
                    let x = origin.x + k as f32 * step * zoom;
                    painter.vline(x.round() + 0.5, area.y_range(), line(k));
                }
                let k0 = ((area.min.y - origin.y) / zoom / step).ceil() as i64;
                let k1 = ((area.max.y - origin.y) / zoom / step).floor() as i64;
                for k in k0..=k1 {
                    let y = origin.y + k as f32 * step * zoom;
                    painter.hline(area.x_range(), y.round() + 0.5, line(k));
                }
            }
        }
        let drag = self.aids.guide_drag;
        if self.prefs.show_guides || drag.is_some() {
            let ink = if self.prefs.lock_guides {
                GUIDE_INK.gamma_multiply(0.6)
            } else {
                GUIDE_INK
            };
            let draw = |vertical: bool, pos: f32, ink: Color32| {
                let st = Stroke::new(1.0, ink);
                if vertical {
                    painter.vline((origin.x + pos * zoom).round() + 0.5, clip.y_range(), st);
                } else {
                    painter.hline(clip.x_range(), (origin.y + pos * zoom).round() + 0.5, st);
                }
            };
            for (i, g) in self.editor.doc().guides.iter().enumerate() {
                if drag.is_some_and(|d| d.index == Some(i)) {
                    continue;
                }
                if self.prefs.show_guides {
                    draw(g.is_vertical(), g.pos, ink);
                }
            }
            if let Some(d) = drag {
                draw(d.vertical, d.pos, GUIDE_INK);
            }
        }
    }

    /// The accent line(s) a snapping drag landed on.
    pub(crate) fn paint_snap_hint(&self, painter: &egui::Painter, clip: egui::Rect, doc_rect: egui::Rect) {
        if self.aids.lines.is_none() {
            return;
        }
        let (origin, zoom) = (doc_rect.min, self.zoom);
        let st = Stroke::new(1.0, ACCENT);
        let smart = &self.aids.smart;
        if let Some(x) = self.aids.hint[0].filter(|x| !smart.shows(true, *x)) {
            painter.vline((origin.x + x * zoom).round() + 0.5, clip.y_range(), st);
        }
        if let Some(y) = self.aids.hint[1].filter(|y| !smart.shows(false, *y)) {
            painter.hline(clip.x_range(), (origin.y + y * zoom).round() + 0.5, st);
        }
    }

    // ---- guides on the canvas ----------------------------------------------------------

    /// The guide under screen point `p`, nearest first.
    fn guide_at(&self, p: Pos2, origin: Pos2) -> Option<usize> {
        let zoom = self.zoom;
        self.editor
            .doc()
            .guides
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let d = if g.is_vertical() {
                    (origin.x + g.pos * zoom - p.x).abs()
                } else {
                    (origin.y + g.pos * zoom - p.y).abs()
                };
                (i, d)
            })
            .filter(|(_, d)| *d <= GUIDE_REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The Move tool grabs guides before layers: hover shows the resize
    /// cursor, a drag moves the guide, and dropping it on a ruler (or off
    /// the canvas area) deletes it. True while guides own the pointer.
    pub(crate) fn guides_canvas_input(&mut self, ctx: &egui::Context, resp: &egui::Response) -> bool {
        let origin = resp.rect.min + self.pan;
        let zoom = self.zoom;
        let primary = egui::PointerButton::Primary;
        let along = |vertical: bool, p: Pos2| {
            if vertical {
                (p.x - origin.x) / zoom
            } else {
                (p.y - origin.y) / zoom
            }
        };
        if let Some(mut d) = self.aids.guide_drag.filter(|d| d.index.is_some()) {
            ctx.set_cursor_icon(if d.vertical {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::ResizeVertical
            });
            if let (true, Some(p)) = (resp.dragged_by(primary), resp.interact_pointer_pos()) {
                d.pos = self.snap_line(d.vertical, along(d.vertical, p));
                self.aids.guide_drag = Some(d);
            }
            if resp.drag_stopped() {
                let at = ctx.input(|i| i.pointer.latest_pos());
                self.finish_guide_drag(at, resp.rect);
            }
            return true;
        }
        let usable = self.tool == Tool::Move
            && self.prefs.show_guides
            && !self.prefs.lock_guides
            && self.drag.is_none();
        if !usable {
            return false;
        }
        // A drag is decided only once the pointer has moved, so grab the
        // guide under the press, not under the pointer now.
        let pressed_on = resp
            .drag_started_by(primary)
            .then(|| ctx.input(|i| i.pointer.press_origin()))
            .flatten()
            .and_then(|p| self.guide_at(p, origin));
        let Some(i) = pressed_on.or_else(|| resp.hover_pos().and_then(|p| self.guide_at(p, origin))) else {
            return false;
        };
        let g = self.editor.doc().guides[i];
        ctx.set_cursor_icon(if g.is_vertical() {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
        if pressed_on.is_some() {
            self.begin_snap(&[]);
            // A guide must not snap to where it already is.
            if let Some(lines) = self.aids.lines.as_mut() {
                let list = if g.is_vertical() {
                    &mut lines.xs
                } else {
                    &mut lines.ys
                };
                if let Some(k) = list.iter().position(|l| *l == (g.pos, SnapSource::Guide)) {
                    list.remove(k);
                }
            }
            self.aids.guide_drag = Some(GuideDrag {
                index: Some(i),
                vertical: g.is_vertical(),
                pos: g.pos,
            });
        } else if resp.drag_started_by(primary) {
            // Pressed elsewhere and dragged over a guide: the tool's drag.
            return false;
        }
        true
    }

    /// Snap a position along one axis (a dragged guide, a crop edge) to
    /// the vertical (x) or horizontal (y) lines.
    pub(crate) fn snap_line(&mut self, vertical: bool, pos: f32) -> f32 {
        let tol = self.snap_tol();
        let Some(lines) = &self.aids.lines else { return pos };
        let s = if vertical {
            lines.snap_x(&[pos], tol)
        } else {
            lines.snap_y(&[pos], tol)
        };
        self.aids.hint = if vertical {
            [s.map(|s| s.line), None]
        } else {
            [None, s.map(|s| s.line)]
        };
        s.map_or(pos, |s| pos + s.delta)
    }

    /// End a guide drag released at `at`: on a ruler or outside the canvas
    /// area it deletes (or never adds) the guide, else it lands there.
    fn finish_guide_drag(&mut self, at: Option<Pos2>, canvas_rect: egui::Rect) {
        let Some(d) = self.aids.guide_drag.take() else {
            return;
        };
        self.end_snap();
        let over_ruler = at.is_some_and(|p| {
            self.prefs.show_rulers && (p.y < canvas_rect.min.y + RULER || p.x < canvas_rect.min.x + RULER)
        });
        let outside = at.is_none_or(|p| !canvas_rect.contains(p));
        let drop = over_ruler || outside;
        let pos = (d.pos * 32.0).round() / 32.0; // PSD keeps 1/32 px
        match (d.index, drop) {
            (Some(index), true) => self.run(&RemoveGuide { index }),
            (Some(index), false) => {
                if self.editor.doc().guides.get(index).is_some_and(|g| g.pos != pos) {
                    self.run(&MoveGuide { index, pos });
                }
            }
            (None, true) => {}
            (None, false) => {
                let guide = if d.vertical {
                    Guide::vertical(pos)
                } else {
                    Guide::horizontal(pos)
                };
                self.run(&AddGuide { guide });
            }
        }
    }

    // ---- rulers ----------------------------------------------------------------------

    /// The rulers along the top and left of the canvas area: ticks in
    /// document pixels that follow zoom and pan, the pointer's position
    /// marked in the accent, and a drag out of either one adds a guide.
    pub(crate) fn rulers_ui(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        self.aids.view = Some(rect);
        if !self.prefs.show_rulers {
            return;
        }
        let origin = rect.min + self.pan;
        let zoom = self.zoom;
        let top = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + RULER, rect.min.y),
            egui::pos2(rect.max.x, rect.min.y + RULER),
        );
        let left = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.min.y + RULER),
            egui::pos2(rect.min.x + RULER, rect.max.y),
        );
        let corner = egui::Rect::from_min_size(rect.min, Vec2::splat(RULER));
        let painter = ui.painter_at(rect);
        for r in [top, left, corner] {
            painter.rect_filled(r, 0.0, PANEL);
        }
        painter.hline(rect.x_range(), top.max.y - 0.5, Stroke::new(1.0, LINE));
        painter.vline(left.max.x - 0.5, rect.y_range(), Stroke::new(1.0, LINE));
        let (step, minor) = ruler_steps(zoom);
        let tick = Stroke::new(1.0, MUTED.gamma_multiply(0.7));
        let font = FontId::monospace(9.0);
        // Top: x ticks.
        {
            let v0 = ((top.min.x - origin.x) / zoom / step).floor() as i64;
            let v1 = ((top.max.x - origin.x) / zoom / step).ceil() as i64;
            for k in v0..=v1 {
                let v = k as f32 * step;
                for j in 0..minor {
                    let x = origin.x + (v + step * j as f32 / minor as f32) * zoom;
                    if x < top.min.x || x > top.max.x {
                        continue;
                    }
                    let len = match j {
                        0 => RULER,
                        j if minor == 10 && j == 5 => 7.0,
                        _ => 4.0,
                    };
                    let x = x.round() + 0.5;
                    painter.vline(x, top.max.y - len..=top.max.y, tick);
                }
                let x = origin.x + v * zoom;
                if x + 2.0 >= top.min.x && x < top.max.x {
                    painter.text(
                        egui::pos2(x + 3.0, top.min.y + 1.0),
                        Align2::LEFT_TOP,
                        ruler_label(v, step),
                        font.clone(),
                        MUTED,
                    );
                }
            }
        }
        // Left: y ticks, labels written downwards one digit per line.
        {
            let v0 = ((left.min.y - origin.y) / zoom / step).floor() as i64;
            let v1 = ((left.max.y - origin.y) / zoom / step).ceil() as i64;
            for k in v0..=v1 {
                let v = k as f32 * step;
                for j in 0..minor {
                    let y = origin.y + (v + step * j as f32 / minor as f32) * zoom;
                    if y < left.min.y || y > left.max.y {
                        continue;
                    }
                    let len = match j {
                        0 => RULER,
                        j if minor == 10 && j == 5 => 7.0,
                        _ => 4.0,
                    };
                    let y = y.round() + 0.5;
                    painter.hline(left.max.x - len..=left.max.x, y, tick);
                }
                let y = origin.y + v * zoom;
                if y + 2.0 >= left.min.y && y < left.max.y {
                    for (n, ch) in ruler_label(v, step).chars().enumerate() {
                        painter.text(
                            egui::pos2(left.min.x + 6.0, y + 2.0 + n as f32 * 8.5),
                            Align2::CENTER_TOP,
                            ch,
                            font.clone(),
                            MUTED,
                        );
                    }
                }
            }
        }
        // Where the pointer is, on both rulers.
        if let Some(p) = ui.ctx().pointer_latest_pos().filter(|p| rect.contains(*p)) {
            let st = Stroke::new(1.0, ACCENT);
            if p.x >= top.min.x {
                painter.vline(p.x.round() + 0.5, top.y_range(), st);
            }
            if p.y >= left.min.y {
                painter.hline(left.x_range(), p.y.round() + 0.5, st);
            }
        }
        // Drag a guide out of a ruler: the top one makes horizontal guides.
        for (r, vertical, salt, name) in [
            (
                top,
                false,
                "ruler-top",
                "Horizontal ruler: drag down to add a guide",
            ),
            (
                left,
                true,
                "ruler-left",
                "Vertical ruler: drag right to add a guide",
            ),
        ] {
            // Pointer-only (no Tab stop): View ▸ New guide is the keyboard way.
            let sense = Sense {
                click: false,
                drag: true,
                focusable: false,
            };
            let resp = ui.interact(r, egui::Id::new(salt), sense);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, name));
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(if vertical {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                });
            }
            let along = |p: Pos2| {
                if vertical {
                    (p.x - origin.x) / zoom
                } else {
                    (p.y - origin.y) / zoom
                }
            };
            if resp.drag_started() {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.begin_snap(&[]);
                    self.aids.guide_drag = Some(GuideDrag {
                        index: None,
                        vertical,
                        pos: along(p),
                    });
                }
            }
            if let (true, Some(p)) = (resp.dragged(), resp.interact_pointer_pos()) {
                let pos = self.snap_line(vertical, along(p));
                if let Some(d) = self.aids.guide_drag.as_mut() {
                    d.pos = pos;
                }
            }
            if resp.drag_stopped() && self.aids.guide_drag.is_some_and(|d| d.index.is_none()) {
                let at = ui.ctx().pointer_latest_pos();
                if !self.prefs.show_guides {
                    // A new guide shows the guides again, as in Photoshop.
                    self.prefs.show_guides = true;
                }
                self.finish_guide_drag(at, rect);
            }
        }
    }

    // ---- actions --------------------------------------------------------------------

    /// Run a View-menu aid action (see [`VIEW_ACTIONS`]).
    pub(crate) fn run_view_aid(&mut self, id: &str) {
        let p = &mut self.prefs;
        let (flag, name) = match id {
            "rulers" => (&mut p.show_rulers, "Rulers"),
            "guides" => (&mut p.show_guides, "Guides"),
            "lock-guides" => (&mut p.lock_guides, "Guide lock"),
            "grid" => (&mut p.show_grid, "Grid"),
            "snap" => (&mut p.snap, "Snapping"),
            "smart-guides" => (&mut p.smart_guides, "Smart guides"),
            "clear-guides" => return self.run(&ClearGuides),
            "new-guide" => {
                let d = self.editor.doc();
                self.dialog = Some(Dialog::NewGuide(false, (d.height as f32 / 2.0).round()));
                return;
            }
            _ => return,
        };
        *flag = !*flag;
        self.status = format!("{name} {}", if *flag { "on" } else { "off" });
        self.prefs.save();
    }

    /// Whether a View-menu aid is on, for its menu check mark.
    pub(crate) fn view_aid_on(&self, id: &str) -> bool {
        match id {
            "rulers" => self.prefs.show_rulers,
            "guides" => self.prefs.show_guides,
            "lock-guides" => self.prefs.lock_guides,
            "grid" => self.prefs.show_grid,
            "snap" => self.prefs.snap,
            "smart-guides" => self.prefs.smart_guides,
            _ => false,
        }
    }
}

impl App {
    /// Debug tokens (`--screenshot-do`) for the crop tool and view aids:
    /// `guides:add=v:X` / `guides:add=h:Y` add a guide; `guides:drag=v:X`
    /// shows a guide being dragged out of a ruler; `snap:hint=X:Y` shows
    /// snap lines as during a drag; `crop:...` (see crop.rs).
    pub(crate) fn debug_crop_guides(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        if self.debug_crop(ctx, tok) {
            return true;
        }
        let parse = |s: &str| -> Option<(bool, f32)> {
            let (axis, v) = s.split_once(':')?;
            Some((axis == "v", v.parse().ok()?))
        };
        if let Some((vertical, pos)) = tok.strip_prefix("guides:add=").and_then(parse) {
            let guide = if vertical {
                Guide::vertical(pos)
            } else {
                Guide::horizontal(pos)
            };
            self.run(&AddGuide { guide });
        } else if let Some((vertical, pos)) = tok.strip_prefix("guides:drag=").and_then(parse) {
            self.begin_snap(&[]);
            self.aids.guide_drag = Some(GuideDrag {
                index: None,
                vertical,
                pos,
            });
        } else if let Some((x, y)) = tok.strip_prefix("snap:hint=").and_then(|s| s.split_once(':')) {
            self.aids.lines = Some(SnapLines::default());
            self.aids.hint = [x.parse().ok(), y.parse().ok()];
        } else {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_ticks_stay_readable_at_every_zoom() {
        assert_eq!(ruler_steps(1.0), (100.0, 10));
        assert_eq!(ruler_steps(0.5), (200.0, 10));
        assert_eq!(ruler_steps(0.25), (500.0, 10));
        assert_eq!(ruler_steps(4.0), (20.0, 10));
        assert_eq!(ruler_steps(0.1), (1000.0, 10));
        // 1/3 zoom: 200 px is 66.7 screen px; tenths would be 6.7 apart.
        assert_eq!(ruler_steps(1.0 / 3.0), (200.0, 10));
        // At 0.7 zoom 100 px is 70 screen px, and tenths 7 apart.
        assert_eq!(ruler_steps(0.7), (100.0, 10));
        assert_eq!(ruler_steps(0.65).1, 10);
        assert_eq!(ruler_steps(0.55), (200.0, 10));
        assert_eq!(ruler_label(1200.0, 100.0), "1200");
        assert_eq!(ruler_label(0.5, 0.5), "0.5");
    }
}

/// Real pointer input through whole frames: rulers, guides, the crop
/// frame and snapping, exercised the way a mouse would.
#[cfg(test)]
mod input_tests {
    use super::*;
    use egui::{Event, PointerButton};

    /// The demo with no dialog and no autosave while frames run.
    fn launch() -> (App, egui::Context) {
        let mut app = App::launch(&["--demo".to_string()]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        (app, ctx)
    }

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<Event>) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Press at `a`, move in steps to `b`, release there.
    fn drag(app: &mut App, ctx: &egui::Context, a: Pos2, b: Pos2) {
        frame(app, ctx, vec![Event::PointerMoved(a)]);
        frame(app, ctx, vec![button(a, true)]);
        for k in 1..=6 {
            let p = a + (b - a) * (k as f32 / 6.0);
            frame(app, ctx, vec![Event::PointerMoved(p)]);
        }
        frame(app, ctx, vec![button(b, false)]);
        frame(app, ctx, vec![]);
    }

    /// Press at the first point, glide through the rest, release at the last.
    fn drag_path(app: &mut App, ctx: &egui::Context, pts: &[Pos2]) {
        frame(app, ctx, vec![Event::PointerMoved(pts[0])]);
        frame(app, ctx, vec![button(pts[0], true)]);
        for w in pts.windows(2) {
            for k in 1..=4 {
                let p = w[0] + (w[1] - w[0]) * (k as f32 / 4.0);
                frame(app, ctx, vec![Event::PointerMoved(p)]);
            }
        }
        frame(app, ctx, vec![button(*pts.last().unwrap(), false)]);
        frame(app, ctx, vec![]);
    }

    #[test]
    fn a_marquee_corner_snaps_to_a_guide() {
        let (mut app, ctx) = launch();
        app.prefs.snap = true;
        app.run(&AddGuide {
            guide: Guide::vertical(700.0),
        });
        app.run(&AddGuide {
            guide: Guide::horizontal(500.0),
        });
        app.tool = Tool::RectSelect;
        frame(&mut app, &ctx, vec![]);
        // From (300, 200) to 3 screen px short of both guides.
        let a = screen(&app, 300.0, 200.0);
        let b = screen(&app, 700.0, 500.0) - egui::vec2(3.0, 3.0);
        drag_path(&mut app, &ctx, &[a, b]);
        let doc = app.editor.doc();
        let r = doc.selection.as_ref().unwrap().tight_bounds(doc.canvas());
        assert_eq!((r.right(), r.bottom()), (700, 500), "{r:?}");
    }

    #[test]
    fn free_transform_moves_snap_to_the_canvas_centre() {
        let (mut app, ctx) = launch();
        app.prefs.snap = true;
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app.run_menu_action("xform");
        assert!(app.xform.is_some());
        frame(&mut app, &ctx, vec![]);
        // Move the whole layer 3 screen px: its centre and edges sit 3 px
        // from the canvas centre and edges, so it snaps back to dx = 0;
        // the drag goes out 40 px first so it counts as a drag.
        let a = screen(&app, 900.0, 600.0);
        drag_path(
            &mut app,
            &ctx,
            &[a, a + egui::vec2(40.0, 0.0), a + egui::vec2(3.0, 0.0)],
        );
        let x = app.xform.as_ref().unwrap();
        assert_eq!(x.dx, 0.0, "snapped home");
        drag_path(&mut app, &ctx, &[a, a + egui::vec2(60.0, 0.0)]);
        let x = app.xform.as_ref().unwrap();
        assert!(x.dx > 50.0 / app.zoom, "a real move: {}", x.dx);
    }

    #[test]
    fn the_move_tool_snaps_a_layer_back_onto_the_canvas_edge() {
        let (mut app, ctx) = launch();
        app.prefs.snap = true;
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app.tool = Tool::Move;
        frame(&mut app, &ctx, vec![]);
        let steps = app.editor.history().len();
        // Out 50 px and back to 3 px off: the left edge snaps back to 0.
        let a = screen(&app, 900.0, 600.0);
        drag_path(
            &mut app,
            &ctx,
            &[a, a + egui::vec2(50.0, 0.0), a + egui::vec2(3.0, 0.0)],
        );
        assert_eq!(app.editor.history().len(), steps, "snapped home: no move at all");
        // A real move goes through.
        drag_path(&mut app, &ctx, &[a, a + egui::vec2(60.0, 0.0)]);
        assert_eq!(app.editor.history().last().copied(), Some("Move"));
        let x = app.editor.doc().layers()[0]
            .pixels()
            .unwrap()
            .content_bounds()
            .unwrap()
            .x;
        assert!(
            x as f32 > 50.0 / app.zoom,
            "moved right by about 60 screen px: {x}"
        );
    }

    /// Document point → screen.
    fn screen(app: &App, x: f32, y: f32) -> Pos2 {
        app.aids.view.unwrap().min + app.pan + egui::vec2(x, y) * app.zoom
    }

    #[test]
    fn a_guide_is_dragged_out_moved_and_dropped_back_on_the_ruler() {
        let (mut app, ctx) = launch();
        app.prefs.show_rulers = true;
        app.prefs.snap = false;
        frame(&mut app, &ctx, vec![]);
        let view = app.aids.view.unwrap();
        // Out of the top ruler down to y = 300: a horizontal guide.
        let start = egui::pos2(view.center().x, view.min.y + RULER / 2.0);
        let to = screen(&app, 900.0, 300.0);
        drag(&mut app, &ctx, start, to);
        let g = app.editor.doc().guides.clone();
        assert_eq!(g.len(), 1, "one guide added");
        assert!(!g[0].is_vertical());
        assert!(
            (g[0].pos - 300.0).abs() <= 1.0 / app.zoom,
            "at y ≈ 300: {}",
            g[0].pos
        );
        assert_eq!(app.editor.history().last().copied(), Some("New guide"));

        // The Move tool drags it to y = 500.
        app.tool = Tool::Move;
        let on = screen(&app, 700.0, g[0].pos);
        let to = screen(&app, 700.0, 500.0);
        drag(&mut app, &ctx, on, to);
        let pos = app.editor.doc().guides[0].pos;
        assert!((pos - 500.0).abs() <= 1.0 / app.zoom, "moved to ≈ 500: {pos}");
        assert_eq!(app.editor.history().last().copied(), Some("Move guide"));

        // Dropped back on the ruler, it is deleted.
        let on = screen(&app, 700.0, pos);
        drag(&mut app, &ctx, on, egui::pos2(on.x, view.min.y + 4.0));
        assert!(app.editor.doc().guides.is_empty());
        assert_eq!(app.editor.history().last().copied(), Some("Delete guide"));

        // Locked guides stay put: the Move tool moves the layer instead.
        app.run(&AddGuide {
            guide: Guide::vertical(400.0),
        });
        app.prefs.lock_guides = true;
        let on = screen(&app, 400.0, 600.0);
        let to = screen(&app, 460.0, 600.0);
        drag(&mut app, &ctx, on, to);
        assert_eq!(app.editor.doc().guides[0].pos, 400.0);
    }

    #[test]
    fn dragging_a_crop_corner_then_enter_crops() {
        let (mut app, ctx) = launch();
        app.prefs.snap = false;
        app.tool = Tool::Crop;
        frame(&mut app, &ctx, vec![]);
        // The top-left handle (the bottom-right one sits by the zoom pill).
        let corner = screen(&app, 0.0, 0.0);
        let to = screen(&app, 300.0, 200.0);
        drag(&mut app, &ctx, corner, to);
        let r = app.crop.frame.unwrap().rect();
        let slack = (1.0 / app.zoom).ceil() as i32;
        assert_eq!((r.right(), r.bottom()), (1800, 1205));
        assert!((r.x - 300).abs() <= slack && (r.y - 200).abs() <= slack, "{r:?}");
        let key = |pressed| Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut app, &ctx, vec![key(true), key(false)]);
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (r.w, r.h));
        assert_eq!(app.editor.history().last().copied(), Some("Crop"));
    }

    #[test]
    fn a_crop_corner_snaps_to_a_guide() {
        let (mut app, ctx) = launch();
        app.prefs.snap = true;
        app.run(&AddGuide {
            guide: Guide::vertical(1000.0),
        });
        app.tool = Tool::Crop;
        frame(&mut app, &ctx, vec![]);
        // Release 3 screen pixels left of the guide: within 6, so it snaps.
        let corner = screen(&app, 0.0, 0.0);
        let near = screen(&app, 1000.0, 300.0) - egui::vec2(3.0, 0.0);
        drag(&mut app, &ctx, corner, near);
        let f = app.crop.frame.unwrap();
        let left = f.cx - f.w / 2.0;
        assert!(
            (left - 1000.0).abs() < 0.01,
            "the left edge sits on the guide: {left}"
        );
        assert!((f.cx + f.w / 2.0 - 1800.0).abs() < 0.01, "the right edge stays");
    }
}
