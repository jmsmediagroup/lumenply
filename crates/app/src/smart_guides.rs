//! Smart Guides (View ▸ Show smart guides, on by default as in Photoshop):
//! while the Move tool drags a layer, magenta lines show where its left,
//! centre or right edge (and top, middle or bottom) lines up with another
//! visible layer's bounds or with the canvas, drawn across both objects,
//! and the drag snaps there. A pill by the pointer reads the move offset.
//! Holding Cmd (Ctrl elsewhere) while dragging turns all snapping off.
//!
//! The maths is the pure [`smart_snap`]; the `App` glue only collects the
//! targets once per drag and adjusts the offset before the drag's single
//! `MoveLayer` command, so nothing here edits the document.

use super::*;

/// Photoshop's Smart Guide magenta.
pub(crate) const SMART_INK: Color32 = Color32::from_rgb(0xFF, 0x2E, 0xD0);

/// Edges closer than this (document pixels) count as lined up, so a
/// centre that sits half a pixel off (odd against even widths) still shows.
const ALIGNED: f32 = 0.5;

/// An axis-aligned box in document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bx {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Bx {
    pub(crate) fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Bx { x0, y0, x1, y1 }
    }

    fn from_rect(r: Rect) -> Self {
        Bx::new(r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32)
    }

    fn shifted(self, dx: f32, dy: f32) -> Self {
        Bx::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }

    /// Left, centre, right.
    fn xs(&self) -> [f32; 3] {
        [self.x0, (self.x0 + self.x1) / 2.0, self.x1]
    }

    /// Top, middle, bottom.
    fn ys(&self) -> [f32; 3] {
        [self.y0, (self.y0 + self.y1) / 2.0, self.y1]
    }
}

/// One alignment line in document pixels: vertical at x = `at` running
/// from y = `from` to `to`, or horizontal at y = `at` from x `from` to `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GuideLine {
    pub vertical: bool,
    pub at: f32,
    pub from: f32,
    pub to: f32,
}

/// Which of the moving box's three lines may meet which of a target's:
/// an edge meets either edge (side by side counts), a centre a centre.
const PAIRS: [(usize, usize); 5] = [(0, 0), (0, 2), (2, 0), (2, 2), (1, 1)];

/// The smallest nudge within `tol` that lines `moving` up with a target.
fn best_nudge(moving: [f32; 3], targets: &[[f32; 3]], tol: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    for t in targets {
        for (i, j) in PAIRS {
            let d = t[j] - moving[i];
            if d.is_finite() && d.abs() <= tol && best.is_none_or(|b| d.abs() < b.abs()) {
                best = Some(d);
            }
        }
    }
    best
}

/// Snap a moving box to other boxes and the canvas, each axis on its own:
/// the nudge `(dx, dy)` (0 where nothing is within `tol`) and the guide
/// lines that show at the snapped position.
pub(crate) fn smart_snap(moving: Bx, others: &[Bx], canvas: Bx, tol: f32) -> (f32, f32, Vec<GuideLine>) {
    let targets: Vec<Bx> = std::iter::once(canvas).chain(others.iter().copied()).collect();
    let xs: Vec<[f32; 3]> = targets.iter().map(Bx::xs).collect();
    let ys: Vec<[f32; 3]> = targets.iter().map(Bx::ys).collect();
    let dx = best_nudge(moving.xs(), &xs, tol).unwrap_or(0.0);
    let dy = best_nudge(moving.ys(), &ys, tol).unwrap_or(0.0);
    let lines = aligned_lines(moving.shifted(dx, dy), &targets, ALIGNED);
    (dx, dy, lines)
}

/// Every line where `moving` already lines up with one of `targets`
/// (within `eps`), each spanning both boxes; lines at the same place merge.
pub(crate) fn aligned_lines(moving: Bx, targets: &[Bx], eps: f32) -> Vec<GuideLine> {
    let mut out: Vec<GuideLine> = Vec::new();
    let mut add = |line: GuideLine| {
        if let Some(l) = out
            .iter_mut()
            .find(|l| l.vertical == line.vertical && (l.at - line.at).abs() < 1e-3)
        {
            l.from = l.from.min(line.from);
            l.to = l.to.max(line.to);
        } else {
            out.push(line);
        }
    };
    for t in targets {
        let (mx, tx, my, ty) = (moving.xs(), t.xs(), moving.ys(), t.ys());
        for (i, j) in PAIRS {
            if (mx[i] - tx[j]).abs() <= eps {
                add(GuideLine {
                    vertical: true,
                    at: tx[j],
                    from: moving.y0.min(t.y0),
                    to: moving.y1.max(t.y1),
                });
            }
            if (my[i] - ty[j]).abs() <= eps {
                add(GuideLine {
                    vertical: false,
                    at: ty[j],
                    from: moving.x0.min(t.x0),
                    to: moving.x1.max(t.x1),
                });
            }
        }
    }
    out
}

/// The painted bounds of every visible layer outside `skip` (groups by
/// their children): what a moving layer can line up with.
fn layer_boxes(doc: &lumenply_doc::Document, skip: &[LayerId]) -> Vec<Bx> {
    fn walk(layers: &[lumenply_doc::Layer], skip: &[LayerId], out: &mut Vec<Bx>) {
        for l in layers {
            if !l.visible || skip.contains(&l.id) {
                continue;
            }
            if let Some(children) = l.children() {
                walk(children, skip, out);
            } else if let Some(b) = l.raster_store().and_then(lumenply_core::snap::painted_bounds) {
                let b = Bx::from_rect(b);
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(doc.layers(), skip, &mut out);
    out
}

/// Transient state of one Move drag (the switch is `Prefs::smart_guides`).
#[derive(Default)]
pub(crate) struct SmartGuides {
    /// Other layers' bounds, collected when the drag began; `None` while
    /// idle or with Smart Guides off.
    targets: Option<Vec<Bx>>,
    /// The lines to draw this frame.
    lines: Vec<GuideLine>,
    /// The moved layer's bounds at its current offset.
    moving: Option<Bx>,
    /// The move so far, for the readout pill.
    offset: Option<(i32, i32)>,
    /// Where the pointer is (screen), to place the pill beside it.
    pub(crate) pointer: Option<Pos2>,
}

impl App {
    /// Move drag starts: collect what the layer can line up with.
    pub(crate) fn begin_smart_guides(&mut self) {
        let skip: Vec<LayerId> = self.active.into_iter().collect();
        let on = self.prefs.smart_guides;
        let s = &mut self.aids.smart;
        *s = SmartGuides::default();
        s.targets = on.then(|| layer_boxes(self.editor.doc(), &skip));
    }

    pub(crate) fn end_smart_guides(&mut self) {
        self.aids.smart = SmartGuides::default();
    }

    /// Smart-snap the Move tool's offset `off` (already snapped by View ▸
    /// Snap where `held` says an axis was): axes the ordinary snapping
    /// took keep its result. `free` (Cmd held) snaps nothing but still
    /// shows exact alignments.
    pub(crate) fn smart_move_offset(&mut self, off: (i32, i32), held: [bool; 2], free: bool) -> (i32, i32) {
        let (Some(b), Some(targets)) = (self.aids.move_bounds, &self.aids.smart.targets) else {
            return off;
        };
        let d = self.editor.doc();
        let canvas = Bx::new(0.0, 0.0, d.width as f32, d.height as f32);
        let at = |o: (i32, i32)| Bx::from_rect(b).shifted(o.0 as f32, o.1 as f32);
        let tol = if free {
            0.0
        } else {
            guides::SNAP_PX / self.zoom.max(1e-4)
        };
        let (dx, dy, _) = smart_snap(at(off), targets, canvas, tol);
        let out = (
            off.0 + if held[0] { 0 } else { dx.round() as i32 },
            off.1 + if held[1] { 0 } else { dy.round() as i32 },
        );
        let all: Vec<Bx> = std::iter::once(canvas).chain(targets.iter().copied()).collect();
        let s = &mut self.aids.smart;
        s.moving = Some(at(out));
        s.lines = aligned_lines(at(out), &all, ALIGNED);
        s.offset = Some(out);
        out
    }

    /// The magenta lines and the offset pill over the canvas.
    pub(crate) fn paint_smart_guides(&self, painter: &egui::Painter, clip: egui::Rect, doc_rect: egui::Rect) {
        let s = &self.aids.smart;
        if s.targets.is_none() {
            return;
        }
        let (origin, zoom) = (doc_rect.min, self.zoom);
        let to_screen = |x: f32, y: f32| origin + egui::vec2(x, y) * zoom;
        let painter = painter.with_clip_rect(clip);
        let st = Stroke::new(1.0, SMART_INK);
        for l in &s.lines {
            if l.vertical {
                let x = to_screen(l.at, 0.0).x.round() + 0.5;
                let (a, b) = (to_screen(0.0, l.from).y, to_screen(0.0, l.to).y);
                painter.vline(x, a..=b, st);
            } else {
                let y = to_screen(0.0, l.at).y.round() + 0.5;
                let (a, b) = (to_screen(l.from, 0.0).x, to_screen(l.to, 0.0).x);
                painter.hline(a..=b, y, st);
            }
        }
        // The readout: beside the pointer, else by the moved layer's corner.
        let (Some(off), Some(m)) = (s.offset, s.moving) else {
            return;
        };
        let anchor = s.pointer.unwrap_or_else(|| to_screen(m.x1, m.y1)) + egui::vec2(16.0, 16.0);
        let text = format!("ΔX: {} px\nΔY: {} px", off.0, off.1);
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(11.5), theme::TEXT);
        let pad = egui::vec2(8.0, 5.0);
        let mut r = egui::Rect::from_min_size(anchor, galley.size() + pad * 2.0);
        // Keep it inside the canvas area.
        r = r.translate(egui::vec2(
            (clip.max.x - 4.0 - r.max.x).min(0.0),
            (clip.max.y - 4.0 - r.max.y).min(0.0),
        ));
        painter.rect(
            r,
            4.0,
            Color32::from_black_alpha(200),
            Stroke::new(1.0, SMART_INK),
        );
        painter.galley(r.min + pad, galley, theme::TEXT);
    }

    /// `guides:smart-drag=X0:Y0:X1:Y1` (document pixels): replay a Move
    /// tool press at (X0, Y0) and a glide to (X1, Y1) without the release,
    /// so the Smart Guides and the readout can be captured mid-drag.
    pub(crate) fn debug_smart_guides(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(arg) = tok.strip_prefix("guides:smart-drag=") else {
            return false;
        };
        let n: Vec<f32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        let (4, Some(view)) = (n.len(), self.aids.view) else {
            eprintln!("guides:smart-drag needs X0:Y0:X1:Y1 and a laid-out canvas");
            return true;
        };
        self.tool = Tool::Move;
        let at = |x: f32, y: f32| view.min + self.pan + egui::vec2(x, y) * self.zoom;
        let (a, b) = (at(n[0], n[1]), at(n[2], n[3]));
        let mut steps = vec![format!("popups:press={}:{}", a.x, a.y)];
        for k in 1..=6 {
            let p = a + (b - a) * (k as f32 / 6.0);
            steps.push(format!("popups:move={}:{}", p.x, p.y));
        }
        for s in steps {
            self.debug_token(ctx, &s);
        }
        true
    }
}

impl SmartGuides {
    /// Whether a Smart Guide already draws this snap line (the plain
    /// accent hint then stays out of its way).
    pub(crate) fn shows(&self, vertical: bool, at: f32) -> bool {
        self.lines
            .iter()
            .any(|l| l.vertical == vertical && (l.at - at).abs() < 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: Bx = Bx {
        x0: 0.0,
        y0: 0.0,
        x1: 1000.0,
        y1: 800.0,
    };

    #[test]
    fn an_edge_snaps_to_another_layers_edge() {
        // Moving 100..200 × 500..560; the other box's left edge is at 203:
        // the moving right edge (200) is 3 px short, so dx = 3, side by side.
        let other = Bx::new(203.0, 100.0, 400.0, 300.0);
        let (dx, dy, lines) = smart_snap(Bx::new(100.0, 500.0, 200.0, 560.0), &[other], CANVAS, 4.0);
        assert_eq!((dx, dy), (3.0, 0.0));
        assert_eq!(
            lines,
            vec![GuideLine {
                vertical: true,
                at: 203.0,
                from: 100.0,
                to: 560.0,
            }]
        );
        // Left edge to left edge, 2 px off upwards.
        let (dx, _, lines) = smart_snap(Bx::new(205.0, 500.0, 255.0, 560.0), &[other], CANVAS, 4.0);
        assert_eq!(dx, -2.0);
        assert_eq!(lines[0].at, 203.0);
    }

    #[test]
    fn centres_snap_to_centres_spanning_both_boxes() {
        // Other centre x = 300; moving 240..340 has centre 290 (10 off),
        // edges far from 200/400: within 12 it snaps by +10.
        let other = Bx::new(200.0, 50.0, 400.0, 150.0);
        let (dx, dy, lines) = smart_snap(Bx::new(240.0, 600.0, 340.0, 700.0), &[other], CANVAS, 12.0);
        assert_eq!((dx, dy), (10.0, 0.0));
        assert_eq!(
            lines,
            vec![GuideLine {
                vertical: true,
                at: 300.0,
                from: 50.0,
                to: 700.0,
            }]
        );
        // A centre never snaps to an edge: centre 205 against left edge 200.
        let (dx, _, _) = smart_snap(Bx::new(155.0, 600.0, 255.0, 700.0), &[other], CANVAS, 6.0);
        assert_eq!(dx, 0.0);
    }

    #[test]
    fn the_canvas_centre_and_edges_are_targets() {
        // 100 × 100 box at 447..547 × 347..447: centre (497, 397) is 3 px
        // from the canvas centre (500, 400) on both axes.
        let (dx, dy, lines) = smart_snap(Bx::new(447.0, 347.0, 547.0, 447.0), &[], CANVAS, 5.0);
        assert_eq!((dx, dy), (3.0, 3.0));
        assert_eq!(
            lines,
            vec![
                GuideLine {
                    vertical: true,
                    at: 500.0,
                    from: 0.0,
                    to: 800.0,
                },
                GuideLine {
                    vertical: false,
                    at: 400.0,
                    from: 0.0,
                    to: 1000.0,
                },
            ]
        );
        // The right edge 2 px past the canvas edge comes back onto it.
        let (dx, dy, _) = smart_snap(Bx::new(902.0, 200.0, 1002.0, 260.0), &[], CANVAS, 5.0);
        assert_eq!((dx, dy), (-2.0, 0.0));
    }

    #[test]
    fn out_of_tolerance_neither_snaps_nor_shows() {
        let other = Bx::new(300.0, 300.0, 400.0, 400.0);
        let (dx, dy, lines) = smart_snap(Bx::new(120.0, 120.0, 194.0, 194.0), &[other], CANVAS, 5.0);
        assert_eq!((dx, dy), (0.0, 0.0));
        assert!(lines.is_empty(), "{lines:?}");
        // Zero tolerance (Cmd held) snaps nothing but shows an exact match.
        let (dx, dy, lines) = smart_snap(Bx::new(300.0, 120.0, 340.0, 160.0), &[other], CANVAS, 0.0);
        assert_eq!((dx, dy), (0.0, 0.0));
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].vertical, lines[0].at, lines[0].from, lines[0].to),
            (true, 300.0, 120.0, 400.0)
        );
    }

    #[test]
    fn x_and_y_snap_independently_to_the_nearest() {
        // x: left edge 3 from 300 (box A) beats right edge 4 from 655 (box B);
        // y: top 2 below box B's bottom (500) snaps up, on its own.
        let a = Bx::new(300.0, 0.0, 380.0, 60.0);
        let b = Bx::new(655.0, 420.0, 760.0, 500.0);
        let (dx, dy, lines) = smart_snap(Bx::new(303.0, 502.0, 651.0, 540.0), &[a, b], CANVAS, 6.0);
        assert_eq!((dx, dy), (-3.0, -2.0));
        assert_eq!(
            lines,
            vec![
                GuideLine {
                    vertical: true,
                    at: 300.0,
                    from: 0.0,
                    to: 538.0,
                },
                GuideLine {
                    vertical: false,
                    at: 500.0,
                    from: 300.0,
                    to: 760.0,
                },
            ]
        );
    }
}

/// Smart Guides driven the way a mouse would: press, glide, release.
#[cfg(test)]
mod input_tests {
    use super::*;
    use egui::{Event, PointerButton};

    /// The demo, Background active, Move tool, View ▸ Snap off so only
    /// Smart Guides can snap; no dialog, no autosave while frames run.
    fn launch() -> (App, egui::Context) {
        let mut app = App::launch(&["--demo".to_string()]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.prefs.snap = false;
        app.prefs.smart_guides = true;
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app.tool = Tool::Move;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        (app, ctx)
    }

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<Event>, modifiers: egui::Modifiers) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    fn press(pos: Pos2, pressed: bool, modifiers: egui::Modifiers) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        }
    }

    /// Press at the first point and glide through the rest with `m` held;
    /// release at the last when `release`.
    fn drag(app: &mut App, ctx: &egui::Context, pts: &[Pos2], m: egui::Modifiers, release: bool) {
        frame(app, ctx, vec![Event::PointerMoved(pts[0])], m);
        frame(app, ctx, vec![press(pts[0], true, m)], m);
        for w in pts.windows(2) {
            for k in 1..=4 {
                let p = w[0] + (w[1] - w[0]) * (k as f32 / 4.0);
                frame(app, ctx, vec![Event::PointerMoved(p)], m);
            }
        }
        if release {
            frame(app, ctx, vec![press(*pts.last().unwrap(), false, m)], m);
            frame(app, ctx, vec![], m);
        }
    }

    fn screen(app: &App, x: f32, y: f32) -> Pos2 {
        app.aids.view.unwrap().min + app.pan + egui::vec2(x, y) * app.zoom
    }

    fn background_x(app: &App) -> i32 {
        app.editor.doc().layers()[0]
            .pixels()
            .unwrap()
            .content_bounds()
            .unwrap()
            .x
    }

    /// Out 50 screen px left and back to 3 px off the canvas edge (and
    /// centre). Leftwards, as the title's kicker line is centred 1 px
    /// right of the canvas centre and would win a move to the right.
    fn out_and_back(app: &App) -> [Pos2; 3] {
        let a = screen(app, 900.0, 600.0);
        [a, a - egui::vec2(50.0, 0.0), a - egui::vec2(3.0, 0.0)]
    }

    #[test]
    fn a_smart_guide_snaps_the_move_back_onto_the_canvas() {
        let (mut app, ctx) = launch();
        let steps = app.editor.history().len();
        let pts = out_and_back(&app);
        drag(&mut app, &ctx, &pts, egui::Modifiers::NONE, true);
        assert_eq!(app.editor.history().len(), steps, "snapped home: no move at all");
        assert_eq!(background_x(&app), 0);
        assert!(app.aids.smart.targets.is_none(), "idle after the release");
    }

    #[test]
    fn mid_drag_the_guides_show_and_the_readout_counts() {
        let (mut app, ctx) = launch();
        let pts = out_and_back(&app);
        drag(&mut app, &ctx, &pts, egui::Modifiers::NONE, false);
        let s = &app.aids.smart;
        assert_eq!(s.offset, Some((0, 0)));
        // Left, centre and right of a full-canvas layer on the canvas's.
        let mut xs: Vec<f32> = s.lines.iter().filter(|l| l.vertical).map(|l| l.at).collect();
        xs.sort_by(f32::total_cmp);
        assert_eq!(xs, vec![0.0, 900.0, 1800.0]);
        // Out at +50 screen px the readout said so (no line within reach).
        let (mut app, ctx) = launch();
        let a = screen(&app, 900.0, 600.0);
        drag(
            &mut app,
            &ctx,
            &[a, a + egui::vec2(50.0, 0.0)],
            egui::Modifiers::NONE,
            false,
        );
        let want = (50.0 / app.zoom).round() as i32;
        assert_eq!(app.aids.smart.offset, Some((want, 0)));
        assert!(app.aids.smart.lines.iter().all(|l| !l.vertical));
    }

    #[test]
    fn cmd_held_or_smart_guides_off_moves_freely() {
        let cmd = egui::Modifiers {
            command: true,
            mac_cmd: cfg!(target_os = "macos"),
            ctrl: !cfg!(target_os = "macos"),
            ..Default::default()
        };
        for (smart, m) in [(true, cmd), (false, egui::Modifiers::NONE)] {
            let (mut app, ctx) = launch();
            app.prefs.smart_guides = smart;
            let pts = out_and_back(&app);
            drag(&mut app, &ctx, &pts, m, true);
            assert_eq!(
                app.editor.history().last().copied(),
                Some("Move"),
                "smart {smart}"
            );
            assert_eq!(
                background_x(&app),
                (-3.0 / app.zoom).round() as i32,
                "smart {smart}"
            );
        }
    }

    #[test]
    fn arrow_nudges_never_snap() {
        let (mut app, _ctx) = launch();
        app.nudge(1, 0);
        assert_eq!(background_x(&app), 1);
    }

    #[test]
    fn the_view_menu_switch_turns_smart_guides_off_and_on() {
        let (mut app, _ctx) = launch();
        app.run_menu_action("smart-guides");
        assert!(!app.prefs.smart_guides);
        assert!(!app.view_aid_on("smart-guides"));
        app.run_menu_action("smart-guides");
        assert!(app.prefs.smart_guides);
    }
}
