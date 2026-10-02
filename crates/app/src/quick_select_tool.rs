//! Quick Selection, the Magic Wand's sibling (Shift+W switches, as in
//! Photoshop): paint over an object and the selection grows to its edges
//! (`lumenply_core::quick_select`). A stroke in New mode replaces the
//! selection and the tool then adds, so further strokes grow it; Alt
//! subtracts, Shift adds.

use super::*;
use crate::options_bar::{bar_slider, select_ops};
use lumenply_core::quick_select::QuickSelect;

/// Quick Selection's own state: its New/Add/Subtract mode, brush size
/// (document pixels) and the stroke being painted.
pub(crate) struct QuickSelectState {
    pub(crate) on: bool,
    pub(crate) op: CombineOp,
    pub(crate) radius: f32,
    points: Vec<(f32, f32)>,
    trail: Vec<Pos2>,
}

impl Default for QuickSelectState {
    fn default() -> Self {
        QuickSelectState {
            on: false,
            op: CombineOp::Replace,
            radius: 20.0,
            points: Vec::new(),
            trail: Vec::new(),
        }
    }
}

impl App {
    /// Canvas input while the Wand tool is in Quick Selection mode.
    pub(crate) fn quick_select_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        let down = resp.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_down());
        if down {
            if let Some(p) = resp.interact_pointer_pos() {
                let d = to_doc(p);
                let qs = &mut self.quick;
                let far = qs.trail.last().is_none_or(|q| q.distance(p) >= 2.0);
                if far {
                    qs.points.push(d);
                    qs.trail.push(p);
                }
            }
            ctx.request_repaint();
            return;
        }
        if self.quick.points.is_empty() {
            return;
        }
        let points = std::mem::take(&mut self.quick.points);
        self.quick.trail.clear();
        let sample = match (self.sample_merged, self.active, self.active_is_pixel()) {
            (false, Some(layer), true) => SampleSource::Layer(layer),
            _ => SampleSource::Merged,
        };
        let mods = ctx.input(|i| i.modifiers);
        let has_sel = self.editor.doc().selection.is_some();
        let op = if mods.alt {
            CombineOp::Subtract
        } else if mods.shift {
            CombineOp::Union
        } else if self.quick.op == CombineOp::Replace && has_sel {
            // New mode starts one selection, then grows it.
            CombineOp::Union
        } else {
            self.quick.op
        };
        let t = std::time::Instant::now();
        self.run(&QuickSelect {
            points,
            radius: self.quick.radius,
            op,
            sample,
        });
        if self.quick.op == CombineOp::Replace {
            self.quick.op = CombineOp::Union;
        }
        self.status = format!("Quick selection ({:.0} ms)", t.elapsed().as_secs_f32() * 1000.0);
    }

    /// The brush circle and the stroke trail being painted.
    pub(crate) fn paint_quick_select(&self, painter: &egui::Painter, resp: &egui::Response) {
        let r = (self.quick.radius * self.zoom).max(2.0);
        if self.quick.trail.len() > 1 {
            painter.add(egui::Shape::line(
                self.quick.trail.clone(),
                Stroke::new(r * 2.0, ACCENT.gamma_multiply(0.25)),
            ));
        }
        if let Some(p) = resp.hover_pos() {
            painter.circle_stroke(p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
            painter.circle_stroke(p, r, Stroke::new(1.0, Color32::WHITE));
            // Photoshop's cursor: a plus inside (adding) or minus (Alt).
            let alt = resp.ctx.input(|i| i.modifiers.alt);
            let s = Stroke::new(1.5, Color32::WHITE);
            painter.line_segment([p - egui::vec2(4.0, 0.0), p + egui::vec2(4.0, 0.0)], s);
            if !alt {
                painter.line_segment([p - egui::vec2(0.0, 4.0), p + egui::vec2(0.0, 4.0)], s);
            }
        }
    }

    /// The Wand tool's options bar in Quick Selection mode.
    pub(crate) fn quick_select_bar(&mut self, ui: &mut egui::Ui) {
        select_ops(ui, &mut self.quick.op);
        ui.separator();
        let mut size = self.quick.radius * 2.0;
        if bar_slider(ui, "Size", &mut size, 2.0..=600.0, " px", true) {
            self.quick.radius = size / 2.0;
        }
        check(ui, &mut self.sample_merged, "All layers")
            .on_hover_text("Sample the merged image instead of the active layer only");
    }

    /// Magic Wand ⇄ Quick Selection, as the first control of the Wand bar.
    pub(crate) fn wand_mode_switch(&mut self, ui: &mut egui::Ui) {
        let mut quick = self.quick.on;
        segmented(
            ui,
            &mut quick,
            &[(false, "Magic wand"), (true, "Quick selection")],
        );
        if quick != self.quick.on {
            self.quick.on = quick;
            self.quick.points.clear();
            self.quick.trail.clear();
        }
        ui.separator();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn halves() -> App {
        let mut doc = Document::new(80, 40);
        let id = doc.add_pixel_layer("halves");
        for y in 0..40 {
            for x in 0..80 {
                let p = if x < 40 {
                    lumenply_tiles::Rgba::new(0.8, 0.1, 0.1, 1.0)
                } else {
                    lumenply_tiles::Rgba::new(0.1, 0.2, 0.8, 1.0)
                };
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, p);
            }
        }
        let mut app = App::launch(&[]);
        app.open_in_new_tab(Editor::new(doc), None);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_active(Some(id));
        app.tool = Tool::Wand;
        app.quick.on = true;
        app.quick.radius = 3.0;
        app
    }

    /// One stroke at `(x, y)` (document pixels; the test canvas maps
    /// points straight through) as pointer input over two frames.
    fn stroke(app: &mut App, x: f32, y: f32, alt: bool) {
        let ctx = egui::Context::default();
        let to_doc = |p: Pos2| (p.x, p.y);
        let mods = egui::Modifiers {
            alt,
            ..Default::default()
        };
        let pos = egui::pos2(x, y);
        let frame = |ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                events,
                modifiers: mods,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 100.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let resp = ui.allocate_rect(ui.max_rect(), Sense::click_and_drag());
                    app.quick_select_input(ctx, &resp, to_doc);
                });
            });
        };
        frame(&ctx, app, vec![egui::Event::PointerMoved(pos)]);
        frame(
            &ctx,
            app,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: mods,
            }],
        );
        frame(&ctx, app, vec![]);
        frame(
            &ctx,
            app,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: mods,
            }],
        );
        frame(&ctx, app, vec![]);
    }

    fn sel(app: &App, x: i32, y: i32) -> f32 {
        app.editor.doc().selection.as_ref().map_or(0.0, |s| s.value(x, y))
    }

    #[test]
    fn strokes_select_then_grow_then_subtract() {
        let mut app = halves();
        // The canvas panel insets content by a few points; aim well inside.
        stroke(&mut app, 20.0, 20.0, false);
        assert!(sel(&app, 5, 5) > 0.95 && sel(&app, 75, 5) < 0.05, "red half");
        assert_eq!(app.quick.op, CombineOp::Union, "New mode turns into Add");
        stroke(&mut app, 60.0, 20.0, false);
        assert!(sel(&app, 5, 5) > 0.95 && sel(&app, 75, 5) > 0.95, "both halves");
        stroke(&mut app, 20.0, 20.0, true);
        assert!(
            sel(&app, 5, 5) < 0.05 && sel(&app, 75, 5) > 0.95,
            "Alt subtracts red"
        );
        let quick_steps = app
            .editor
            .history()
            .iter()
            .filter(|l| **l == "Quick selection")
            .count();
        assert_eq!(quick_steps, 3, "one undo step per stroke");
    }
}
