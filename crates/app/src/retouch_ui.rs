//! Photoshop's retouching tools, as modes of the tools they belong to:
//! Heal ▸ Spot / Healing / Patch / Red Eye and Eraser ▸ Eraser /
//! Background / Magic. Options-bar controls, canvas interaction, overlays
//! and `retouch:...` debug tokens live here; the edits themselves are
//! commands in `lumenply_core` (retouch.rs).

use super::*;

/// What the Heal tool does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum HealMode {
    /// Spot healing: paint, and the surroundings fill in.
    #[default]
    Spot,
    /// Healing brush: texture from an Alt+clicked source, tone from around.
    Healing,
    /// Patch: lasso the defect, drag it onto clean texture.
    Patch,
    /// Red eye: click a red pupil, or drag a box around the eye.
    RedEye,
}

impl HealMode {
    pub(crate) const ALL: [(HealMode, &'static str); 4] = [
        (HealMode::Spot, "Spot"),
        (HealMode::Healing, "Healing"),
        (HealMode::Patch, "Patch"),
        (HealMode::RedEye, "Red Eye"),
    ];
}

/// What the Eraser does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum EraserMode {
    #[default]
    Eraser,
    /// Erase colours close to the one under the brush centre.
    Background,
    /// Click: erase the contiguous area of similar colour.
    Magic,
}

impl EraserMode {
    pub(crate) const ALL: [(EraserMode, &'static str); 3] = [
        (EraserMode::Eraser, "Eraser"),
        (EraserMode::Background, "Background"),
        (EraserMode::Magic, "Magic"),
    ];
}

/// A patch being dragged: where the press landed (document space) and how
/// far the patch has moved so far.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PatchDrag {
    pub(crate) from: (f32, f32),
    pub(crate) offset: (i32, i32),
}

/// Options and in-progress state of the retouching modes.
pub(crate) struct Retouch {
    pub(crate) heal_mode: HealMode,
    pub(crate) eraser_mode: EraserMode,
    /// Patch: synthesise with PatchMatch instead of healing the copy.
    pub(crate) patch_aware: bool,
    pub(crate) patch_drag: Option<PatchDrag>,
    /// Red eye: the press of a box being dragged (document space).
    pub(crate) eye_from: Option<(f32, f32)>,
    /// Red eye options, in percent.
    pub(crate) pupil: f32,
    pub(crate) darken: f32,
    /// Debug: centre the view on this document point at this zoom.
    pub(crate) focus: Option<(f32, f32, f32)>,
    /// The history brush paints from this state; `None` = the oldest kept
    /// ("Open").
    pub(crate) history_source: Option<HistorySource>,
    /// Background eraser: tolerance in percent, resampling under every
    /// dab, and erasing only what connects to the brush centre.
    pub(crate) bg_tolerance: f32,
    pub(crate) bg_continuous: bool,
    pub(crate) bg_contiguous: bool,
}

/// A history state picked as the history brush's source: its step index
/// (for display) and the document as it was then, kept even if the step
/// later falls off the history.
#[derive(Clone)]
pub(crate) struct HistorySource {
    pub(crate) step: usize,
    pub(crate) label: String,
    pub(crate) doc: Document,
}

impl Retouch {
    /// Forget per-document state (the document was replaced).
    pub(crate) fn reset(&mut self) {
        self.patch_drag = None;
        self.eye_from = None;
        self.history_source = None;
    }
}

impl Default for Retouch {
    fn default() -> Self {
        Retouch {
            heal_mode: HealMode::Spot,
            eraser_mode: EraserMode::Eraser,
            patch_aware: false,
            patch_drag: None,
            eye_from: None,
            pupil: 50.0,
            darken: 50.0,
            focus: None,
            history_source: None,
            bg_tolerance: 25.0,
            bg_continuous: true,
            bg_contiguous: true,
        }
    }
}

/// Side of the box a single red-eye click searches: the brush diameter,
/// at least 8 px.
fn eye_box(center: (f32, f32), radius: f32) -> Rect {
    let side = (2.0 * radius).max(8.0);
    Rect::new(
        (center.0 - side / 2.0).floor() as i32,
        (center.1 - side / 2.0).floor() as i32,
        side.ceil() as u32,
        side.ceil() as u32,
    )
}

/// A labelled mode control: segmented on a wide bar, a menu otherwise.
fn mode_control<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    wide: bool,
    id: &str,
    name: &str,
    value: &mut T,
    modes: &[(T, &'static str)],
) {
    if wide {
        segmented(ui, value, modes);
    } else {
        let current = modes.iter().find(|(m, _)| m == value).map_or("", |(_, l)| *l);
        let r = egui::ComboBox::from_id_salt(id)
            .selected_text(current)
            .width(92.0)
            .show_ui(ui, |ui| {
                popup_style(ui);
                for &(m, label) in modes {
                    ui.selectable_value(value, m, label);
                }
            })
            .response;
        a11y_name(&r, name);
        r.on_hover_text(name);
    }
}

impl App {
    /// History step the history brush paints from (0 = "Open").
    pub(crate) fn history_source_step(&self) -> usize {
        self.retouch.history_source.as_ref().map_or(0, |s| s.step)
    }

    /// Make history step `step` the history brush's source.
    pub(crate) fn set_history_source(&mut self, step: usize) {
        let labels: Vec<String> = std::iter::once("Open".to_string())
            .chain(self.editor.history().iter().map(|s| s.to_string()))
            .chain(self.editor.redo_history().iter().map(|s| s.to_string()))
            .collect();
        if let (Some(doc), Some(label)) = (self.editor.state(step), labels.get(step)) {
            self.retouch.history_source = Some(HistorySource {
                step,
                label: label.clone(),
                doc: doc.clone(),
            });
            self.status = format!("History brush source: step {step} ({label})");
        }
    }

    /// A history-brush stroke on `layer`, painting from the source state.
    pub(crate) fn history_stroke(
        &self,
        layer: LayerId,
        brush: Brush,
        points: Vec<StrokePoint>,
    ) -> HistoryStroke {
        let doc = match &self.retouch.history_source {
            Some(s) => Some(&s.doc),
            None => self.editor.state(0),
        };
        let source = doc.and_then(|d| d.layer(layer)).and_then(|l| l.pixels()).cloned();
        HistoryStroke {
            layer,
            brush,
            points,
            source,
        }
    }

    /// The history brush's source picker for the Brush bar.
    pub(crate) fn history_source_ui(&mut self, ui: &mut egui::Ui) {
        let current = match &self.retouch.history_source {
            Some(s) => format!("{} · {}", s.step, s.label),
            None => "Open".to_string(),
        };
        let labels: Vec<String> = std::iter::once("Open".to_string())
            .chain(self.editor.history().iter().map(|s| s.to_string()))
            .collect();
        let mut pick = None;
        ui.label(RichText::new("From").color(MUTED));
        let r = egui::ComboBox::from_id_salt("history-source")
            .selected_text(current)
            .width(140.0)
            .show_ui(ui, |ui| {
                popup_style(ui);
                let at = self.history_source_step();
                for (i, label) in labels.iter().enumerate() {
                    if ui.selectable_label(i == at, format!("{i} · {label}")).clicked() {
                        pick = Some(i);
                    }
                }
            })
            .response;
        a11y_name(&r, "History brush source");
        r.on_hover_text("The history state the brush paints back (also: right-click a History card)");
        if let Some(i) = pick {
            self.set_history_source(i);
        }
    }

    /// The sample source for retouching commands on `layer`.
    fn retouch_sample(&self, layer: LayerId) -> SampleSource {
        if self.sample_merged {
            SampleSource::Merged
        } else {
            SampleSource::Layer(layer)
        }
    }

    /// Mode controls at the head of the Heal and Eraser bars, plus the
    /// whole bar for modes that are not brushes (Patch, Red Eye, Magic
    /// eraser). Returns true when the bar is complete, so the brush
    /// sliders that follow are skipped.
    pub(crate) fn retouch_options_bar(&mut self, ui: &mut egui::Ui, wide: bool) -> bool {
        match self.tool {
            Tool::Heal => {
                mode_control(
                    ui,
                    wide,
                    "heal-mode",
                    "Healing mode",
                    &mut self.retouch.heal_mode,
                    &HealMode::ALL,
                );
                ui.separator();
                match self.retouch.heal_mode {
                    HealMode::Patch => {
                        check(ui, &mut self.retouch.patch_aware, "Content-Aware").on_hover_text(
                            "Synthesise the patch from the dragged-to area (PatchMatch) instead of healing a copy of it",
                        );
                        check(ui, &mut self.sample_merged, "All layers").on_hover_text(
                            "Take the texture from the merged image instead of the active layer",
                        );
                        if wide {
                            ui.label(
                                RichText::new("Draw around the flaw, then drag it onto clean texture").weak(),
                            );
                        }
                        true
                    }
                    HealMode::RedEye => {
                        crate::options_bar::bar_slider(
                            ui,
                            "Pupil size",
                            &mut self.retouch.pupil,
                            0.0..=100.0,
                            "%",
                            false,
                        );
                        crate::options_bar::bar_slider(
                            ui,
                            "Darken",
                            &mut self.retouch.darken,
                            0.0..=100.0,
                            "%",
                            false,
                        );
                        let mut size = self.brush.radius * 2.0;
                        if crate::options_bar::bar_slider(ui, "Box", &mut size, 8.0..=400.0, " px", true) {
                            self.brush.radius = size / 2.0;
                        }
                        if wide {
                            ui.label(RichText::new("Click a red pupil, or drag a box around the eye").weak());
                        }
                        true
                    }
                    HealMode::Spot | HealMode::Healing => false,
                }
            }
            Tool::Eraser => {
                mode_control(
                    ui,
                    wide,
                    "eraser-mode",
                    "Eraser mode",
                    &mut self.retouch.eraser_mode,
                    &EraserMode::ALL,
                );
                ui.separator();
                match self.retouch.eraser_mode {
                    EraserMode::Eraser => false,
                    EraserMode::Background => {
                        crate::options_bar::bar_slider(
                            ui,
                            "Tolerance",
                            &mut self.retouch.bg_tolerance,
                            0.0..=100.0,
                            "%",
                            false,
                        );
                        segmented(
                            ui,
                            &mut self.retouch.bg_continuous,
                            &[(false, "Once"), (true, "Continuous")],
                        );
                        check(ui, &mut self.retouch.bg_contiguous, "Contiguous")
                            .on_hover_text("Erase only what connects to the brush centre");
                        ui.separator();
                        false
                    }
                    EraserMode::Magic => {
                        let mut tol = self.tolerance * 100.0;
                        if crate::options_bar::bar_slider(ui, "Tolerance", &mut tol, 0.0..=100.0, "%", false)
                        {
                            self.tolerance = tol / 100.0;
                        }
                        check(ui, &mut self.contiguous, "Contiguous")
                            .on_hover_text("Erase only the connected area of similar colour");
                        check(ui, &mut self.sample_merged, "All layers")
                            .on_hover_text("Judge colours on the merged image instead of the active layer");
                        let mut op = self.brush.color[3] * 100.0;
                        if crate::options_bar::bar_slider(ui, "Opacity", &mut op, 1.0..=100.0, "%", false) {
                            self.brush.color[3] = op / 100.0;
                        }
                        if wide {
                            ui.label(RichText::new("Click a colour to erase it").weak());
                        }
                        true
                    }
                }
            }
            _ => false,
        }
    }

    /// The palette's retouching-mode actions (`tool-patch`, ...): pick the
    /// tool and its mode. False for any other id.
    pub(crate) fn retouch_tool_action(&mut self, id: &str) -> bool {
        match id {
            "tool-spot-heal" => self.retouch.heal_mode = HealMode::Spot,
            "tool-patch" => self.retouch.heal_mode = HealMode::Patch,
            "tool-red-eye" => self.retouch.heal_mode = HealMode::RedEye,
            "tool-blur" => self.brush.mode = BrushMode::Blur,
            "tool-sharpen" => self.brush.mode = BrushMode::Sharpen,
            "tool-history-brush" => self.brush.mode = BrushMode::History,
            "tool-bg-eraser" => self.retouch.eraser_mode = EraserMode::Background,
            "tool-magic-eraser" => self.retouch.eraser_mode = EraserMode::Magic,
            _ => return false,
        }
        self.tool = match id {
            "tool-spot-heal" | "tool-patch" | "tool-red-eye" => Tool::Heal,
            "tool-bg-eraser" | "tool-magic-eraser" => Tool::Eraser,
            _ => Tool::Brush,
        };
        true
    }

    /// A Background-eraser stroke with the bar's options.
    pub(crate) fn background_erase(
        &self,
        layer: LayerId,
        brush: Brush,
        points: Vec<StrokePoint>,
    ) -> BackgroundErase {
        BackgroundErase {
            layer,
            brush,
            points,
            tolerance: self.retouch.bg_tolerance / 100.0,
            continuous: self.retouch.bg_continuous,
            contiguous: self.retouch.bg_contiguous,
        }
    }

    /// Magic eraser: a click erases the similar-coloured area under it.
    fn magic_eraser_canvas(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: &dyn Fn(Pos2) -> (f32, f32),
    ) {
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if !resp.clicked_by(egui::PointerButton::Primary) {
            return;
        }
        let Some(p) = resp.interact_pointer_pos() else {
            return;
        };
        let (x, y) = to_doc(p);
        match (self.retouch_block(), self.active) {
            (Some(why), _) => self.status = why.into(),
            (None, Some(layer)) => {
                let cmd = self.magic_erase_command(layer, x.floor() as i32, y.floor() as i32);
                self.run(&cmd);
            }
            _ => {}
        }
    }

    fn magic_erase_command(&self, layer: LayerId, x: i32, y: i32) -> MagicErase {
        MagicErase {
            layer,
            x,
            y,
            tolerance: self.tolerance,
            contiguous: self.contiguous,
            sample: self.retouch_sample(layer),
            opacity: self.brush.color[3],
        }
    }

    /// Canvas input for the retouching modes that are not plain strokes.
    /// Returns true when it handled the frame.
    pub(crate) fn retouch_canvas(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: &dyn Fn(Pos2) -> (f32, f32),
    ) -> bool {
        if let Some((x, y, z)) = self.retouch.focus.take() {
            self.zoom = z;
            self.pan = resp.rect.size() / 2.0 - egui::vec2(x, y) * z;
        }
        match (self.tool, self.retouch.heal_mode) {
            (Tool::Heal, HealMode::Patch) => self.patch_canvas(ctx, resp, to_doc),
            (Tool::Heal, HealMode::RedEye) => self.red_eye_canvas(ctx, resp, to_doc),
            (Tool::Eraser, _) if self.retouch.eraser_mode == EraserMode::Magic => {
                self.magic_eraser_canvas(ctx, resp, to_doc)
            }
            _ => return false,
        }
        true
    }

    /// Why the active layer can't take a retouching edit, if it can't.
    fn retouch_block(&self) -> Option<&'static str> {
        if let Some(why) = self.lock_block(layer_actions::LockNeed::Paint) {
            return Some(why);
        }
        (!self.active_is_pixel()).then_some("Select a pixel layer to retouch")
    }

    fn patch_command(&self, layer: LayerId, offset: (i32, i32), content_aware: bool) -> PatchHeal {
        PatchHeal {
            layer,
            offset,
            sample: self.retouch_sample(layer),
            content_aware,
        }
    }

    /// Patch: a drag outside the selection draws a freehand lasso (it
    /// becomes the selection); a drag inside it moves the patch, previewing
    /// the healed result, and the release applies it.
    fn patch_canvas(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: &dyn Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        let inside = |doc: &Document, p: (f32, f32)| {
            doc.selection
                .as_ref()
                .is_some_and(|s| s.value(p.0.floor() as i32, p.1.floor() as i32) > 0.0)
        };
        if let Some(h) = resp.hover_pos() {
            let grab = self.retouch.patch_drag.is_some() || inside(self.editor.doc(), to_doc(h));
            ctx.set_cursor_icon(if grab {
                egui::CursorIcon::Move
            } else {
                egui::CursorIcon::Crosshair
            });
        }
        if resp.drag_started_by(primary) {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                let d = to_doc(p);
                if inside(self.editor.doc(), d) {
                    match self.retouch_block() {
                        Some(why) => self.status = why.into(),
                        None => {
                            self.retouch.patch_drag = Some(PatchDrag {
                                from: d,
                                offset: (0, 0),
                            });
                            self.drag = Some(DragKind::Retouch);
                        }
                    }
                } else {
                    self.drag = Some(DragKind::Lasso);
                    self.lasso.clear();
                    self.lasso.push(d);
                }
            }
        }
        if resp.dragged_by(primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let q = to_doc(p);
                if self.drag == Some(DragKind::Lasso) {
                    let far = self
                        .lasso
                        .last()
                        .is_none_or(|l| (l.0 - q.0).abs() + (l.1 - q.1).abs() > 0.5);
                    if far {
                        self.lasso.push(q);
                    }
                }
                if let (Some(mut pd), Some(layer)) = (self.retouch.patch_drag, self.active) {
                    let off = ((q.0 - pd.from.0).round() as i32, (q.1 - pd.from.1).round() as i32);
                    if off != pd.offset {
                        pd.offset = off;
                        self.retouch.patch_drag = Some(pd);
                        // Preview the healed copy (content-aware runs on
                        // release only: it takes too long per frame).
                        let cmd = self.patch_command(layer, off, false);
                        let mut preview = self.editor.doc().clone();
                        if cmd.apply(&mut preview).is_ok() {
                            let area = cmd.affected(self.editor.doc());
                            self.preview(ctx, &preview, area);
                        } else {
                            self.mark(None);
                        }
                    }
                }
            }
        }
        if resp.drag_stopped() {
            match self.drag {
                Some(DragKind::Lasso) => {
                    self.drag = None;
                    self.finish_polygon(ctx);
                }
                Some(DragKind::Retouch) => {
                    self.drag = None;
                    let pd = self.retouch.patch_drag.take();
                    match (pd, self.active) {
                        (Some(pd), Some(layer)) if pd.offset != (0, 0) => {
                            let cmd = self.patch_command(layer, pd.offset, self.retouch.patch_aware);
                            self.run(&cmd);
                        }
                        _ => self.mark(None),
                    }
                }
                _ => {}
            }
        } else if resp.clicked_by(primary) {
            let on_sel = resp
                .interact_pointer_pos()
                .is_some_and(|p| inside(self.editor.doc(), to_doc(p)));
            if !on_sel && self.editor.doc().selection.is_some() {
                self.run(&SetSelection { selection: None });
            }
        }
    }

    fn red_eye_command(&self, layer: LayerId, area: Rect) -> RedEye {
        RedEye {
            layer,
            area,
            pupil_size: self.retouch.pupil / 100.0,
            darken: self.retouch.darken / 100.0,
        }
    }

    /// Red eye: a click searches a brush-sized box around the pointer; a
    /// drag searches the box dragged out.
    fn red_eye_canvas(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: &dyn Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.drag_started_by(primary) {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                self.retouch.eye_from = Some(to_doc(p));
                self.drag = Some(DragKind::Retouch);
            }
        }
        let area = if resp.drag_stopped() && self.drag == Some(DragKind::Retouch) {
            self.drag = None;
            let end = resp
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
            match (self.retouch.eye_from.take(), end) {
                (Some(a), Some(b)) => Some(drag_rect(a, to_doc(b), self.editor.doc().canvas())),
                _ => None,
            }
        } else if resp.clicked_by(primary) {
            resp.interact_pointer_pos()
                .map(|p| eye_box(to_doc(p), self.brush.radius))
        } else {
            None
        };
        if let Some(area) = area.filter(|a| !a.is_empty()) {
            match (self.retouch_block(), self.active) {
                (Some(why), _) => self.status = why.into(),
                (None, Some(layer)) => self.run(&self.red_eye_command(layer, area)),
                _ => {}
            }
        }
    }

    /// Overlays for the retouching modes; true when one was drawn (the
    /// default brush circle is then skipped).
    pub(crate) fn retouch_overlay(
        &self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        resp: &egui::Response,
    ) -> bool {
        let origin = resp.rect.min + self.pan;
        let zoom = self.zoom;
        let ts = |x: f32, y: f32| egui::pos2(origin.x + x * zoom, origin.y + y * zoom);
        let dashed = |pts: Vec<Pos2>| {
            painter.add(Shape::line(pts.clone(), Stroke::new(1.0, Color32::WHITE)));
            painter.extend(Shape::dashed_line(
                &pts,
                Stroke::new(1.0, Color32::BLACK),
                5.0,
                5.0,
            ));
        };
        match (self.tool, self.retouch.heal_mode) {
            (Tool::Heal, HealMode::Patch) => {
                if self.drag == Some(DragKind::Lasso) && !self.lasso.is_empty() {
                    let mut pts: Vec<Pos2> = self.lasso.iter().map(|&(x, y)| ts(x, y)).collect();
                    pts.push(pts[0]);
                    dashed(pts);
                }
                if let Some(pd) = self.retouch.patch_drag {
                    // The patch outline, carried to where it samples from.
                    let (dx, dy) = (pd.offset.0 as f32, pd.offset.1 as f32);
                    let px = zoom.max(1.0);
                    for &(x, y) in &self.sel_points {
                        let min = ts(x as f32 + dx, y as f32 + dy);
                        painter.rect_filled(egui::Rect::from_min_size(min, Vec2::splat(px)), 0.0, ACCENT);
                    }
                }
                true
            }
            (Tool::Heal, HealMode::RedEye) => {
                let r = match (self.retouch.eye_from, ctx.input(|i| i.pointer.latest_pos())) {
                    (Some(a), Some(b)) if self.drag == Some(DragKind::Retouch) => {
                        Some(egui::Rect::from_two_pos(ts(a.0, a.1), b))
                    }
                    _ => resp.hover_pos().map(|p| {
                        let side = (2.0 * self.brush.radius).max(8.0) * zoom;
                        egui::Rect::from_center_size(p, Vec2::splat(side))
                    }),
                };
                if let Some(r) = r {
                    dashed(vec![
                        r.left_top(),
                        r.right_top(),
                        r.right_bottom(),
                        r.left_bottom(),
                        r.left_top(),
                    ]);
                    let c = r.center();
                    painter.line_segment(
                        [c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)],
                        Stroke::new(1.0, Color32::WHITE),
                    );
                    painter.line_segment(
                        [c - egui::vec2(0.0, 4.0), c + egui::vec2(0.0, 4.0)],
                        Stroke::new(1.0, Color32::WHITE),
                    );
                }
                true
            }
            // The magic eraser is a click, not a brush: no circle.
            (Tool::Eraser, _) if self.retouch.eraser_mode == EraserMode::Magic => true,
            (Tool::Eraser, _) if self.retouch.eraser_mode == EraserMode::Background => {
                // The sampling hot spot at the brush centre.
                if let Some(c) = resp.hover_pos() {
                    for (a, b) in [((-4.0, 0.0), (4.0, 0.0)), ((0.0, -4.0), (0.0, 4.0))] {
                        let seg = [c + egui::vec2(a.0, a.1), c + egui::vec2(b.0, b.1)];
                        painter.line_segment(seg, Stroke::new(3.0, Color32::from_black_alpha(140)));
                        painter.line_segment(seg, Stroke::new(1.0, Color32::WHITE));
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Debug tokens (`retouch:...`) for headless screenshots:
    /// `retouch:heal=patch` / `eraser=magic` pick a mode (by its label,
    /// lower case, spaces as dashes); `retouch:view=X:Y:ZOOM` centres the
    /// view on a document point; `retouch:patch=DX:DY` and
    /// `retouch:patch-ca=DX:DY` patch the selection from that offset;
    /// `retouch:drag=DX:DY` shows a patch drag in progress;
    /// `retouch:dot=X:Y:R` paints a red disc (a stand-in red eye);
    /// `retouch:redeye=X:Y:W:H` fixes red eye in that box;
    /// `retouch:brush=blur` (paint / blur / sharpen / history) picks a brush
    /// mode, `retouch:radius=R` the brush radius, `retouch:source=N` the
    /// history brush's source step; `retouch:stroke=X0:Y0:X1:Y1` runs the
    /// current tool's stroke along that line; `retouch:magic=X:Y` runs the
    /// magic eraser there.
    pub(crate) fn debug_retouch(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("retouch:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        let slug = |s: &str| s.to_ascii_lowercase().replace(' ', "-");
        let layer = self.active;
        match (verb, nums.as_slice()) {
            ("heal", _) => {
                self.tool = Tool::Heal;
                if let Some(&(m, _)) = HealMode::ALL.iter().find(|(_, l)| slug(l) == arg) {
                    self.retouch.heal_mode = m;
                }
            }
            ("eraser", _) => {
                self.tool = Tool::Eraser;
                if let Some(&(m, _)) = EraserMode::ALL.iter().find(|(_, l)| slug(l) == arg) {
                    self.retouch.eraser_mode = m;
                }
            }
            ("brush", _) => {
                self.tool = Tool::Brush;
                let modes = [
                    (BrushMode::Paint, "paint"),
                    (BrushMode::Blur, "blur"),
                    (BrushMode::Sharpen, "sharpen"),
                    (BrushMode::History, "history"),
                ];
                if let Some(&(m, _)) = modes.iter().find(|(_, l)| *l == arg) {
                    self.brush.mode = m;
                }
            }
            ("radius", &[r]) => self.brush.radius = r,
            ("magic", &[x, y]) => {
                if let Some(layer) = layer {
                    let cmd = self.magic_erase_command(layer, x as i32, y as i32);
                    self.run(&cmd);
                }
            }
            ("source", &[n]) => self.set_history_source(n as usize),
            ("stroke", &[x0, y0, x1, y1]) => {
                if let Some(layer) = layer {
                    let points: Vec<StrokePoint> = (0..=16)
                        .map(|i| {
                            let t = i as f32 / 16.0;
                            StrokePoint::new(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, 1.0)
                        })
                        .collect();
                    let t = std::time::Instant::now();
                    let cmd = self.stroke_command(layer, points);
                    self.run(cmd.as_ref());
                    eprintln!(
                        "{} took {:.3} s ({})",
                        cmd.label(),
                        t.elapsed().as_secs_f32(),
                        self.status
                    );
                }
            }
            ("view", &[x, y, z]) => self.retouch.focus = Some((x, y, z)),
            ("patch" | "patch-ca", &[dx, dy]) => {
                if let Some(layer) = layer {
                    let t = std::time::Instant::now();
                    let cmd = self.patch_command(layer, (dx as i32, dy as i32), verb == "patch-ca");
                    self.run(&cmd);
                    eprintln!(
                        "{} took {:.3} s ({})",
                        cmd.label(),
                        t.elapsed().as_secs_f32(),
                        self.status
                    );
                }
            }
            ("drag", &[dx, dy]) => {
                if let Some(layer) = layer {
                    let offset = (dx as i32, dy as i32);
                    self.retouch.patch_drag = Some(PatchDrag {
                        from: (0.0, 0.0),
                        offset,
                    });
                    let cmd = self.patch_command(layer, offset, false);
                    let mut preview = self.editor.doc().clone();
                    if cmd.apply(&mut preview).is_ok() {
                        let area = cmd.affected(self.editor.doc());
                        self.refresh(ctx);
                        self.preview(ctx, &preview, area);
                    }
                }
            }
            ("dot", &[x, y, r]) => {
                if let Some(layer) = layer {
                    let brush = Brush {
                        radius: r,
                        hardness: 0.85,
                        color: linear_rgba([0.78, 0.06, 0.05], 1.0),
                        spacing: 0.1,
                        jitter: 0.0,
                        mode: BrushMode::Paint,
                    };
                    self.run(&PaintStroke {
                        layer,
                        brush,
                        points: vec![StrokePoint::new(x, y, 1.0)],
                    });
                }
            }
            ("redeye", &[x, y, w, h]) => {
                if let Some(layer) = layer {
                    let area = Rect::new(x as i32, y as i32, w as u32, h as u32);
                    self.run(&self.red_eye_command(layer, area));
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
    use crate::a11y_tests::{ctx, launch, nameless};

    /// A small document whose only layer is a pixel layer.
    /// Opened (no history yet) like a file: the "Open" state has the layer.
    fn small_app() -> App {
        let mut app = launch(&[]);
        let mut doc = Document::new(48, 48);
        let id = doc.add_pixel_layer("bg");
        let fill = Raster::filled(48, 48, lumenply_tiles::Rgba::new(0.2, 0.3, 0.4, 1.0));
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() =
            lumenply_tiles::TileStore::from_raster(&fill, 0, 0);
        app.open_in_new_tab(Editor::new(doc), None);
        app
    }

    #[test]
    fn the_history_brush_paints_from_the_picked_step() {
        let mut app = small_app();
        let ctx = ctx();
        let layer = app.active.expect("the layer is active");
        app.run(&Fill {
            layer,
            color: [1.0, 0.0, 0.0, 1.0],
        });
        app.run(&Fill {
            layer,
            color: [0.0, 0.0, 1.0, 1.0],
        });
        app.tool = Tool::Brush;
        app.brush.mode = BrushMode::History;
        app.brush.hardness = 1.0;
        app.brush.radius = 4.0;
        app.brush.color[3] = 1.0;
        assert_eq!(
            nameless(&mut app, &ctx),
            Vec::<String>::new(),
            "history brush bar"
        );
        let at = |app: &App| {
            app.editor
                .doc()
                .layer(layer)
                .unwrap()
                .pixels()
                .unwrap()
                .get_pixel(24, 24)
        };
        // By default it paints the opened image back.
        let cmd = app.stroke_command(layer, vec![StrokePoint::new(24.0, 24.0, 1.0)]);
        assert_eq!(cmd.label(), "History brush");
        app.run(cmd.as_ref());
        let p = at(&app);
        assert!((p.r - 0.2).abs() < 1e-3 && (p.b - 0.4).abs() < 1e-3, "{p:?}");
        // Step 1 is the red fill.
        app.set_history_source(1);
        assert_eq!(app.history_source_step(), 1);
        let cmd = app.stroke_command(layer, vec![StrokePoint::new(24.0, 24.0, 1.0)]);
        app.run(cmd.as_ref());
        let p = at(&app);
        assert!((p.r - 1.0).abs() < 1e-3 && p.b.abs() < 1e-3, "{p:?}");
        // Blur and sharpen are plain paint strokes.
        app.brush.mode = BrushMode::Blur;
        let cmd = app.stroke_command(layer, vec![StrokePoint::new(24.0, 24.0, 1.0)]);
        assert_eq!(cmd.label(), "Blur");
        // Another document drops the source.
        app.retouch.reset();
        assert_eq!(app.history_source_step(), 0);
    }

    #[test]
    fn every_heal_mode_names_its_controls() {
        let mut app = small_app();
        let ctx = ctx();
        app.tool = Tool::Heal;
        for (mode, label) in HealMode::ALL {
            app.retouch.heal_mode = mode;
            assert_eq!(
                nameless(&mut app, &ctx),
                Vec::<String>::new(),
                "heal mode {label}"
            );
        }
    }

    #[test]
    fn every_eraser_mode_names_its_controls_and_erases() {
        let mut app = small_app();
        let ctx = ctx();
        app.tool = Tool::Eraser;
        for (mode, label) in EraserMode::ALL {
            app.retouch.eraser_mode = mode;
            assert_eq!(
                nameless(&mut app, &ctx),
                Vec::<String>::new(),
                "eraser mode {label}"
            );
        }
        let layer = app.active.unwrap();
        // Magic: the uniform layer goes in one click, one undo step.
        let cmd = app.magic_erase_command(layer, 5, 5);
        app.run(&cmd);
        assert_eq!(app.editor.history().last().copied(), Some("Magic eraser"));
        assert!(app
            .editor
            .doc()
            .layer(layer)
            .unwrap()
            .pixels()
            .unwrap()
            .is_empty());
        app.run_menu_action("undo");
        // Background: a stroke erases the sampled colour under the brush.
        app.retouch.eraser_mode = EraserMode::Background;
        let cmd = app.stroke_command(layer, vec![StrokePoint::new(24.0, 24.0, 1.0)]);
        assert_eq!(cmd.label(), "Background eraser");
        app.run(cmd.as_ref());
        let px = app.editor.doc().layer(layer).unwrap().pixels().unwrap();
        assert_eq!(px.get_pixel(24, 24).a, 0.0);
        assert!((px.get_pixel(2, 2).a - 1.0).abs() < 1e-4);
    }

    #[test]
    fn palette_actions_pick_the_tool_and_its_mode() {
        let mut app = small_app();
        app.run_menu_action("tool-patch");
        assert_eq!((app.tool, app.retouch.heal_mode), (Tool::Heal, HealMode::Patch));
        app.run_menu_action("tool-history-brush");
        assert_eq!((app.tool, app.brush.mode), (Tool::Brush, BrushMode::History));
        app.run_menu_action("tool-magic-eraser");
        assert_eq!(
            (app.tool, app.retouch.eraser_mode),
            (Tool::Eraser, EraserMode::Magic)
        );
        assert!(!app.retouch_tool_action("tool-nonsense"));
    }

    #[test]
    fn a_red_eye_click_box_follows_the_brush() {
        assert_eq!(eye_box((20.0, 30.0), 10.0), Rect::new(10, 20, 20, 20));
        assert_eq!(eye_box((5.5, 5.5), 1.0), Rect::new(1, 1, 8, 8));
    }

    #[test]
    fn the_patch_debug_token_heals_the_selection_in_one_step() {
        let mut app = small_app();
        let ctx = ctx();
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(4, 4, 8, 8))),
        });
        let steps = app.editor.history().len();
        assert!(app.debug_retouch(&ctx, "retouch:patch=20:20"));
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last().copied(), Some("Patch"));
    }
}
