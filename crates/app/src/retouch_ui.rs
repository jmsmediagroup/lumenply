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
    /// Content-aware move: lasso something, drag it elsewhere; the gap
    /// fills itself.
    Move,
}

impl HealMode {
    pub(crate) const ALL: [(HealMode, &'static str); 5] = [
        (HealMode::Spot, "Spot"),
        (HealMode::Healing, "Healing"),
        (HealMode::Patch, "Patch"),
        (HealMode::Move, "Move"),
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
    /// Patch: the selection is the clean texture, dragged onto the flaw
    /// (Photoshop's Destination); off, it is the flaw (Source).
    pub(crate) patch_destination: bool,
    /// Content-aware move: heal the moved pixels into their new place.
    pub(crate) move_adapt: bool,
    /// What Patch and Spot healing read (Photoshop's Sample menu).
    pub(crate) sample: RetouchSample,
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
    /// Background eraser: keep the foreground (brush) colour.
    pub(crate) bg_protect: bool,
    /// Spot healing synthesises the stroke's area with PatchMatch on
    /// release (the live preview stays the fast diffusion heal).
    pub(crate) spot_aware: bool,
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
            patch_destination: false,
            move_adapt: false,
            sample: RetouchSample::Current,
            patch_drag: None,
            eye_from: None,
            pupil: 50.0,
            darken: 50.0,
            focus: None,
            history_source: None,
            bg_tolerance: 25.0,
            bg_continuous: true,
            bg_contiguous: true,
            bg_protect: false,
            spot_aware: true,
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

/// Photoshop's Sample menu for Patch and Spot healing.
fn sample_menu(ui: &mut egui::Ui, value: &mut RetouchSample) {
    const OPTIONS: [(RetouchSample, &str); 3] = [
        (RetouchSample::Current, "Current layer"),
        (RetouchSample::CurrentAndBelow, "Current & below"),
        (RetouchSample::All, "All layers"),
    ];
    let current = OPTIONS.iter().find(|(m, _)| m == value).map_or("", |(_, l)| *l);
    let r = egui::ComboBox::from_id_salt("retouch-sample")
        .selected_text(current)
        .width(118.0)
        .show_ui(ui, |ui| {
            popup_style(ui);
            for (m, label) in OPTIONS {
                ui.selectable_value(value, m, label);
            }
        })
        .response;
    a11y_name(&r, "Sample");
    r.on_hover_text(
        "What the heal reads. Below or All lets you retouch on an empty layer and keep the photo untouched",
    );
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
        // A state with another canvas size (before a crop, say) can't be
        // painted back pixel for pixel.
        let same_size = |d: &&Document| d.canvas() == self.editor.doc().canvas();
        let source = doc
            .filter(same_size)
            .and_then(|d| d.layer(layer))
            .and_then(|l| l.pixels())
            .cloned();
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
    pub(crate) fn retouch_sample(&self, layer: LayerId) -> SampleSource {
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
    pub(crate) fn retouch_options_bar(&mut self, ui: &mut egui::Ui, tier: crate::options_bar::Tier) -> bool {
        let wide = tier == crate::options_bar::Tier::Wide;
        // Tight bars drop the sliders and keep the scrubbable numbers.
        let tight = tier == crate::options_bar::Tier::Tight;
        let slider =
            |ui: &mut egui::Ui, label: &str, v: &mut f32, r: RangeInclusive<f32>, suffix: &str, log: bool| {
                if tight {
                    crate::options_bar::bar_value(ui, label, v, r, suffix)
                } else {
                    crate::options_bar::bar_slider(ui, label, v, r, suffix, log)
                }
            };
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
                        let dest = &mut self.retouch.patch_destination;
                        if wide {
                            segmented(ui, dest, &[(false, "Source"), (true, "Destination")]);
                        } else {
                            check(ui, dest, "Destination");
                        }
                        check(ui, &mut self.retouch.patch_aware, "Content-Aware").on_hover_text(
                            "Synthesise the patch from the dragged-to area (PatchMatch) instead of healing a copy of it",
                        );
                        sample_menu(ui, &mut self.retouch.sample);
                        if wide {
                            let hint = if self.retouch.patch_destination {
                                "Draw around clean texture, then drag it onto the flaw"
                            } else {
                                "Draw around the flaw, then drag it onto clean texture"
                            };
                            ui.label(RichText::new(hint).weak());
                        }
                        true
                    }
                    HealMode::Move => {
                        check(ui, &mut self.retouch.move_adapt, "Adapt").on_hover_text(
                            "Heal the moved pixels into their new surroundings (tone follows the new place)",
                        );
                        if wide {
                            ui.label(
                                RichText::new("Draw around something, then drag it; the gap fills itself")
                                    .weak(),
                            );
                        }
                        true
                    }
                    HealMode::RedEye => {
                        slider(ui, "Pupil size", &mut self.retouch.pupil, 0.0..=100.0, "%", false);
                        slider(ui, "Darken", &mut self.retouch.darken, 0.0..=100.0, "%", false);
                        let mut size = self.brush.radius * 2.0;
                        if slider(ui, "Box", &mut size, 8.0..=400.0, " px", true) {
                            self.brush.radius = size / 2.0;
                        }
                        if wide {
                            ui.label(RichText::new("Click a red pupil, or drag a box around the eye").weak());
                        }
                        true
                    }
                    HealMode::Spot => {
                        sample_menu(ui, &mut self.retouch.sample);
                        check(ui, &mut self.retouch.spot_aware, "Content-Aware").on_hover_text(
                            "On release, rebuild the stroked area from its surroundings (PatchMatch); \
                             off: blend the surrounding colour in",
                        );
                        ui.separator();
                        false
                    }
                    HealMode::Healing => false,
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
                        slider(
                            ui,
                            "Tolerance",
                            &mut self.retouch.bg_tolerance,
                            0.0..=100.0,
                            "%",
                            false,
                        );
                        if wide {
                            segmented(
                                ui,
                                &mut self.retouch.bg_continuous,
                                &[(false, "Once"), (true, "Continuous")],
                            );
                        } else {
                            check(ui, &mut self.retouch.bg_continuous, "Continuous").on_hover_text(
                                "Resample the colour under every dab (off: once, at the start)",
                            );
                        }
                        check(ui, &mut self.retouch.bg_contiguous, "Contiguous")
                            .on_hover_text("Erase only what connects to the brush centre");
                        check(ui, &mut self.retouch.bg_protect, "Protect FG")
                            .on_hover_text("Never erase colours close to the foreground colour");
                        ui.separator();
                        false
                    }
                    EraserMode::Magic => {
                        let mut tol = self.tolerance * 100.0;
                        if slider(ui, "Tolerance", &mut tol, 0.0..=100.0, "%", false) {
                            self.tolerance = tol / 100.0;
                        }
                        check(ui, &mut self.contiguous, "Contiguous")
                            .on_hover_text("Erase only the connected area of similar colour");
                        check(ui, &mut self.sample_merged, "All layers")
                            .on_hover_text("Judge colours on the merged image instead of the active layer");
                        let mut op = self.brush.color[3] * 100.0;
                        if slider(ui, "Opacity", &mut op, 1.0..=100.0, "%", false) {
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

    /// A tool's key was pressed. With Shift on the Heal or Eraser tool
    /// already active, step to its next mode, as Shift+J and Shift+E cycle
    /// tool groups in Photoshop.
    pub(crate) fn select_tool_key(&mut self, tool: Tool, shift: bool) {
        if shift && tool == self.tool {
            fn next<T: PartialEq + Copy>(cur: T, all: &[(T, &str)]) -> T {
                let i = all.iter().position(|(m, _)| *m == cur).unwrap_or(0);
                all[(i + 1) % all.len()].0
            }
            match tool {
                Tool::Heal => self.retouch.heal_mode = next(self.retouch.heal_mode, &HealMode::ALL),
                Tool::Eraser => self.retouch.eraser_mode = next(self.retouch.eraser_mode, &EraserMode::ALL),
                _ => {}
            }
        }
        self.tool = tool;
    }

    /// The palette's retouching-mode actions (`tool-patch`, ...): pick the
    /// tool and its mode. False for any other id.
    pub(crate) fn retouch_tool_action(&mut self, id: &str) -> bool {
        match id {
            "tool-spot-heal" => self.retouch.heal_mode = HealMode::Spot,
            "tool-patch" => self.retouch.heal_mode = HealMode::Patch,
            "tool-content-move" => self.retouch.heal_mode = HealMode::Move,
            "tool-red-eye" => self.retouch.heal_mode = HealMode::RedEye,
            "tool-blur" => self.brush.mode = BrushMode::Blur,
            "tool-sharpen" => self.brush.mode = BrushMode::Sharpen,
            "tool-history-brush" => self.brush.mode = BrushMode::History,
            "tool-bg-eraser" => self.retouch.eraser_mode = EraserMode::Background,
            "tool-magic-eraser" => self.retouch.eraser_mode = EraserMode::Magic,
            _ => return false,
        }
        self.tool = match id {
            "tool-spot-heal" | "tool-patch" | "tool-content-move" | "tool-red-eye" => Tool::Heal,
            "tool-bg-eraser" | "tool-magic-eraser" => Tool::Eraser,
            _ => Tool::Brush,
        };
        true
    }

    /// A Spot (diffusion) or Healing stroke, healed as one region. Spot
    /// reads what the Sample menu says; Healing follows its All layers box.
    pub(crate) fn heal_region(
        &self,
        layer: LayerId,
        brush: Brush,
        points: Vec<StrokePoint>,
        texture: bool,
    ) -> HealRegion {
        let sample = match (self.retouch.heal_mode, self.sample_merged) {
            (HealMode::Healing, true) => RetouchSample::All,
            (HealMode::Healing, false) => RetouchSample::Current,
            _ => self.retouch.sample,
        };
        HealRegion {
            layer,
            brush,
            points,
            offset: if texture { self.clone_offset } else { (0, 0) },
            texture,
            sample,
        }
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
            protect: self.retouch.bg_protect.then(|| {
                let [r, g, b, _] = linear_rgba(self.brush_rgb, 1.0);
                [r, g, b]
            }),
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
            (Tool::Heal, HealMode::Patch | HealMode::Move) => self.patch_canvas(ctx, resp, to_doc),
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
            sample: self.retouch.sample,
            content_aware,
            destination: self.retouch.patch_destination,
        }
    }

    /// What a drag inside the selection does in Patch or Move mode. The
    /// preview (`commit` false) skips the slow parts: content-aware
    /// synthesis, and Move's fill behind.
    fn drag_command(&self, layer: LayerId, offset: (i32, i32), commit: bool) -> Box<dyn Command> {
        if self.retouch.heal_mode == HealMode::Move {
            Box::new(ContentAwareMove {
                layer,
                offset,
                fill: commit,
                adapt: self.retouch.move_adapt,
            })
        } else {
            Box::new(self.patch_command(layer, offset, commit && self.retouch.patch_aware))
        }
    }

    /// The pixels a Patch or Move drag changes at `offset`, for previews.
    fn drag_area(&self, layer: LayerId, offset: (i32, i32)) -> Option<Rect> {
        let doc = self.editor.doc();
        if self.retouch.heal_mode == HealMode::Move {
            ContentAwareMove {
                layer,
                offset,
                fill: false,
                adapt: false,
            }
            .area(doc)
        } else {
            self.patch_command(layer, offset, false).area(doc)
        }
    }

    /// Patch and Move: a drag outside the selection draws a freehand lasso
    /// (it becomes the selection); a drag inside it carries the selection,
    /// previewing the result, and the release applies it.
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
                        // Repaint where the last preview drew as well.
                        let was = self.drag_area(layer, pd.offset);
                        pd.offset = off;
                        self.retouch.patch_drag = Some(pd);
                        let cmd = self.drag_command(layer, off, false);
                        let mut preview = self.editor.doc().clone();
                        if cmd.apply(&mut preview).is_ok() {
                            let area = match (self.drag_area(layer, off), was) {
                                (Some(a), Some(b)) => Some(a.union(&b)),
                                _ => None,
                            };
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
                            let cmd = self.drag_command(layer, pd.offset, true);
                            self.run(cmd.as_ref());
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
            (Tool::Heal, HealMode::Patch | HealMode::Move) => {
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
    /// magic eraser there; `retouch:sample=current|below|all` picks what
    /// Patch and Spot healing read, `retouch:dest=1` Patch's Destination,
    /// `retouch:aware=0` turns content-aware spot healing off,
    /// `retouch:offset=DX:DY` sets the healing (clone) source offset,
    /// `retouch:move=DX:DY` content-aware moves the selection,
    /// `retouch:adapt=1` heals moved pixels into their new place.
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
            ("sample", _) => {
                self.retouch.sample = match arg {
                    "below" => RetouchSample::CurrentAndBelow,
                    "all" => RetouchSample::All,
                    _ => RetouchSample::Current,
                }
            }
            ("dest", &[v]) => self.retouch.patch_destination = v != 0.0,
            ("aware", &[v]) => self.retouch.spot_aware = v != 0.0,
            ("offset", &[dx, dy]) => {
                self.clone_source = Some((0.0, 0.0));
                self.clone_picking = false;
                self.clone_offset = (dx as i32, dy as i32);
            }
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
            ("move", &[dx, dy]) => {
                if let Some(layer) = layer {
                    self.retouch.heal_mode = HealMode::Move;
                    let t = std::time::Instant::now();
                    let cmd = self.drag_command(layer, (dx as i32, dy as i32), true);
                    self.run(cmd.as_ref());
                    eprintln!(
                        "{} took {:.3} s ({})",
                        cmd.label(),
                        t.elapsed().as_secs_f32(),
                        self.status
                    );
                }
            }
            ("adapt", &[v]) => self.retouch.move_adapt = v != 0.0,
            ("drag", &[dx, dy]) => {
                if let Some(layer) = layer {
                    let offset = (dx as i32, dy as i32);
                    self.retouch.patch_drag = Some(PatchDrag {
                        from: (0.0, 0.0),
                        offset,
                    });
                    let cmd = self.drag_command(layer, offset, false);
                    let mut preview = self.editor.doc().clone();
                    if cmd.apply(&mut preview).is_ok() {
                        let area = self.drag_area(layer, offset);
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
                        ..Brush::default()
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
        // A crop since the source state: nothing lines up any more.
        app.run(&CropDocument {
            rect: Rect::new(0, 0, 40, 40),
        });
        app.brush.mode = BrushMode::History;
        let cmd = app.stroke_command(layer, vec![StrokePoint::new(24.0, 24.0, 1.0)]);
        let steps = app.editor.history().len();
        app.run(cmd.as_ref());
        assert_eq!(app.editor.history().len(), steps, "refused");
        assert!(app.status.contains("canvas size"), "{}", app.status);
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
    fn spot_healing_previews_fast_and_commits_content_aware() {
        let mut app = small_app();
        let layer = app.active.unwrap();
        app.tool = Tool::Heal;
        let label = |app: &App| {
            app.stroke_command(layer, vec![StrokePoint::new(9.0, 9.0, 1.0)])
                .label()
        };
        assert_eq!(label(&app), "Spot heal (content-aware)");
        app.drag = Some(DragKind::Stroke);
        assert_eq!(label(&app), "Spot heal", "the live preview");
        app.drag = None;
        app.retouch.spot_aware = false;
        assert_eq!(label(&app), "Spot heal");
        app.retouch.heal_mode = HealMode::Healing;
        // The healing brush without a picked source heals untextured.
        assert_eq!(label(&app), "Spot heal");
        app.clone_source = Some((30.0, 30.0));
        assert_eq!(label(&app), "Healing brush");
    }

    #[test]
    fn shift_with_the_tool_key_cycles_its_modes() {
        let mut app = small_app();
        app.select_tool_key(Tool::Heal, true);
        assert_eq!(
            app.retouch.heal_mode,
            HealMode::Spot,
            "first press only picks the tool"
        );
        for want in [
            HealMode::Healing,
            HealMode::Patch,
            HealMode::Move,
            HealMode::RedEye,
            HealMode::Spot,
        ] {
            app.select_tool_key(Tool::Heal, true);
            assert_eq!(app.retouch.heal_mode, want);
        }
        app.select_tool_key(Tool::Heal, false);
        assert_eq!(app.retouch.heal_mode, HealMode::Spot, "plain J keeps the mode");
        app.select_tool_key(Tool::Eraser, false);
        app.select_tool_key(Tool::Eraser, true);
        assert_eq!(app.retouch.eraser_mode, EraserMode::Background);
        app.select_tool_key(Tool::Brush, true);
        assert_eq!((app.tool, app.brush.mode), (Tool::Brush, BrushMode::Paint));
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

    /// Real pointer input through whole app frames at 1440×900.
    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    fn button(app: &mut App, ctx: &egui::Context, p: Pos2, pressed: bool) {
        frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }

    fn drag(app: &mut App, ctx: &egui::Context, from: Pos2, to: Pos2) {
        button(app, ctx, from, true);
        for i in 1..=6 {
            let t = i as f32 / 6.0;
            frame(app, ctx, vec![egui::Event::PointerMoved(from + (to - from) * t)]);
        }
        button(app, ctx, to, false);
    }

    /// Screen position of a document point, calibrated by hovering the
    /// canvas centre and reading back the document pixel under it.
    fn to_screen(app: &mut App, ctx: &egui::Context) -> impl Fn(f32, f32) -> Pos2 {
        let probe = egui::pos2(620.0, 450.0);
        frame(app, ctx, vec![egui::Event::PointerMoved(probe)]);
        let (px, py) = app.cursor_doc.expect("the probe is over the document");
        let z = app.zoom;
        move |x, y| {
            egui::pos2(
                probe.x + (x - (px as f32 + 0.5)) * z,
                probe.y + (y - (py as f32 + 0.5)) * z,
            )
        }
    }

    #[test]
    fn dragging_on_the_canvas_lassoes_then_patches() {
        let mut app = small_app();
        let ctx = ctx();
        app.tool = Tool::Heal;
        app.retouch.heal_mode = HealMode::Patch;
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![]);
        }
        let at = to_screen(&mut app, &ctx);
        // A drag outside any selection draws the patch outline.
        let steps = app.editor.history().len();
        button(&mut app, &ctx, at(6.0, 6.0), true);
        for (x, y) in [(18.0, 6.0), (18.0, 18.0), (6.0, 18.0), (6.0, 7.0)] {
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at(x, y))]);
        }
        button(&mut app, &ctx, at(6.0, 7.0), false);
        assert_eq!(
            app.editor.history().len(),
            steps + 1,
            "the lasso became a selection"
        );
        let sel = app.editor.doc().selection.clone().expect("selected");
        assert!(sel.value(12, 12) > 0.99 && sel.value(30, 30) == 0.0);
        // A drag from inside it moves the patch; the release heals it in.
        drag(&mut app, &ctx, at(12.0, 12.0), at(32.0, 30.0));
        assert_eq!(app.editor.history().last().copied(), Some("Patch"));
        assert_eq!(app.editor.history().len(), steps + 2);
        assert!(app.retouch.patch_drag.is_none() && app.drag.is_none());
    }

    #[test]
    fn dragging_a_selection_in_move_mode_moves_it_and_fills_behind() {
        let mut app = small_app();
        let ctx = ctx();
        let layer = app.active.unwrap();
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(8, 8, 8, 8))),
        });
        app.run(&Fill {
            layer,
            color: [1.0, 0.0, 0.0, 1.0],
        });
        app.tool = Tool::Heal;
        app.retouch.heal_mode = HealMode::Move;
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![]);
        }
        let at = to_screen(&mut app, &ctx);
        drag(&mut app, &ctx, at(12.0, 12.0), at(32.0, 30.0));
        assert_eq!(app.editor.history().last().copied(), Some("Content-aware move"));
        let px = app.editor.doc().layer(layer).unwrap().pixels().unwrap();
        // The red square sits 20 right and 18 down; the blue-grey around
        // it fills in behind.
        let moved = px.get_pixel(32, 30);
        assert!((moved.r - 1.0).abs() < 1e-3 && moved.g.abs() < 1e-3, "{moved:?}");
        let behind = px.get_pixel(12, 12);
        assert!(
            (behind.r - 0.2).abs() < 1e-3 && (behind.b - 0.4).abs() < 1e-3,
            "{behind:?}"
        );
        // The marching ants follow the selection to its new place.
        frame(&mut app, &ctx, vec![]);
        assert!(!app.sel_points.is_empty());
        assert!(
            app.sel_points.iter().all(|&(x, y)| x >= 26 && y >= 24),
            "stale outline"
        );
    }

    #[test]
    fn a_red_eye_click_on_the_canvas_fixes_the_pupil() {
        let mut app = small_app();
        let ctx = ctx();
        let layer = app.active.unwrap();
        app.run(&PaintStroke {
            layer,
            brush: Brush {
                radius: 5.0,
                hardness: 1.0,
                color: [0.8, 0.02, 0.02, 1.0],
                spacing: 0.2,
                jitter: 0.0,
                mode: BrushMode::Paint,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(24.0, 24.0, 1.0)],
        });
        app.tool = Tool::Heal;
        app.retouch.heal_mode = HealMode::RedEye;
        app.brush.radius = 10.0;
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![]);
        }
        let at = to_screen(&mut app, &ctx);
        button(&mut app, &ctx, at(24.0, 24.0), true);
        button(&mut app, &ctx, at(24.0, 24.0), false);
        assert_eq!(app.editor.history().last().copied(), Some("Red eye"));
        let p = app
            .editor
            .doc()
            .layer(layer)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(24, 24);
        let [r, g, b, _] = p.to_straight();
        assert!(r < 0.05 && (r - g).abs() < 1e-3 && (g - b).abs() < 1e-3, "{p:?}");
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
