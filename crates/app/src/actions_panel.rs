//! Window ▸ Actions: record a sequence of edits, replay it on any document
//! as one undo step, and keep actions in `actions.json` in the user data
//! folder. The model and the player live in `lumenply_core::actions`
//! (ADR 0023); this file is the floating panel, the recorder and the app's
//! playback host.
//!
//! Recording hooks two places: the action registry
//! ([`App::run_menu_action`] calls [`App::record_menu`]) and the dialog
//! confirmations ([`App::record_dialog`]), plus adding an adjustment layer.
//! Every frame [`App::actions_sync`] turns history entries that no hook
//! claimed into "not recordable yet" notes, so nothing is silently lost.

use std::path::PathBuf;

use lumenply_core::actions::{self, Action, ActionHost, Step};

use super::*;
use crate::navigator::float_header;
use crate::panels::float_frame;

/// Registry ids (menus, palette, keys).
pub(crate) const ACTIONS_PANEL: &str = "actions-panel";
pub(crate) const ACTION_RECORD: &str = "action-record";
pub(crate) const ACTION_STOP: &str = "action-stop";
pub(crate) const ACTION_PLAY: &str = "action-play";

/// The panel's width.
const PANEL_W: f32 = 260.0;

/// One action in the list, with whether its steps are shown.
pub(crate) struct Entry {
    pub(crate) action: Action,
    pub(crate) open: bool,
}

/// An action being recorded.
struct Recorder {
    /// Index of the action the steps go into.
    target: usize,
    /// History length already accounted for.
    seen: usize,
    /// The document being recorded (a tab switch re-bases `seen`).
    doc_key: u64,
    /// A step was just recorded: the history entries its command pushes
    /// belong to it, not to "not recordable" notes.
    claim: bool,
    /// The adjustment layer the last `Adjust` step added: its settings are
    /// re-read until the next step, so Properties tweaks land in the step.
    adjust: Option<LayerId>,
}

/// The Actions panel's state (actions themselves persist in actions.json).
#[derive(Default)]
pub(crate) struct ActionsState {
    pub(crate) shown: bool,
    pub(crate) list: Vec<Entry>,
    pub(crate) selected: Option<usize>,
    recording: Option<Recorder>,
    renaming: Option<(usize, String)>,
    confirm_delete: Option<usize>,
    /// The last outcome shown at the panel's foot: (is an error, text).
    pub(crate) message: Option<(bool, String)>,
    /// Set by [`App::run`] when a command fails: playback reads it.
    pub(crate) edit_error: Option<String>,
    /// Above zero while a recorded registry action or a playback runs:
    /// nested hooks (auto colour adds a Levels layer) record nothing.
    mute: u32,
    /// Where user actions are saved; `None` never saves.
    path: Option<PathBuf>,
    /// actions.json existed but could not be read: never overwrite it.
    load_failed: bool,
}

impl ActionsState {
    /// The built-ins plus the user's actions from the data folder.
    pub(crate) fn load() -> Self {
        Self::load_from(session::data_dir().map(|d| d.join("actions.json")))
    }

    pub(crate) fn load_from(path: Option<PathBuf>) -> Self {
        let mut s = ActionsState {
            list: actions::builtin_actions()
                .into_iter()
                .map(|action| Entry { action, open: false })
                .collect(),
            path,
            ..Default::default()
        };
        if let Some(text) = s.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            match actions::from_json(&text) {
                Ok(user) => s
                    .list
                    .extend(user.into_iter().map(|action| Entry { action, open: false })),
                Err(e) => {
                    s.load_failed = true;
                    s.message = Some((true, format!("actions.json not loaded: {e}")));
                }
            }
        }
        s
    }

    /// Write the user's actions (built-ins are never written).
    fn save(&mut self) {
        let Some(path) = self.path.clone() else { return };
        if self.load_failed {
            self.message = Some((
                true,
                "Not saved: fix or remove the unreadable actions.json first".into(),
            ));
            return;
        }
        let list: Vec<Action> = self.list.iter().map(|e| e.action.clone()).collect();
        let res = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, actions::to_json(&list)));
        if let Err(e) = res {
            self.message = Some((true, format!("Could not save actions: {e}")));
        }
    }

    pub(crate) fn recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Index of the action being recorded.
    pub(crate) fn recording_into(&self) -> Option<usize> {
        self.recording.as_ref().map(|r| r.target)
    }

    fn unique_name(&self, base: &str) -> String {
        (1..)
            .map(|n| format!("{base} {n}"))
            .find(|name| !self.list.iter().any(|e| &e.action.name == name))
            .expect("some number is free")
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.list
            .iter()
            .position(|e| e.action.name.eq_ignore_ascii_case(name))
    }
}

/// The app as a playback host: registry steps run through the same runner
/// as the menus, so a played step behaves exactly like the menu item.
struct AppHost<'a> {
    app: &'a mut App,
}

impl ActionHost for AppHost<'_> {
    fn editor(&mut self) -> &mut Editor {
        &mut self.app.editor
    }

    fn active_layer(&self) -> Option<LayerId> {
        self.app
            .active
            .filter(|id| self.app.editor.doc().layer(*id).is_some())
    }

    fn set_active_layer(&mut self, id: Option<LayerId>) {
        self.app.set_active(id);
        self.app.fix_active();
    }

    fn run_menu(&mut self, id: &str) -> Result<(), String> {
        if let Some(why) = self.app.action_block(id) {
            return Err(why.into());
        }
        if matches!(id, "auto-contrast" | "auto-color") {
            // They read the composite; earlier steps changed it.
            self.app.last_flat = Some(lumenply_render::composite_raster(self.app.editor.doc()));
        }
        self.app.actions.edit_error = None;
        self.app.run_menu_action_unrecorded(id);
        match self.app.actions.edit_error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl App {
    // ---- recording hooks ---------------------------------------------------

    /// Record `step` if recording (and not inside a recorded action).
    /// Call it *before* the step's command runs.
    pub(crate) fn record_step(&mut self, step: Step) {
        if self.actions.mute > 0 || !self.actions.recording() {
            return;
        }
        self.actions_sync();
        let Some(rec) = self.actions.recording.as_mut() else {
            return;
        };
        rec.claim = true;
        rec.adjust = None;
        if let Some(e) = self.actions.list.get_mut(rec.target) {
            e.action.steps.push(step);
        }
    }

    /// Adding an adjustment layer (`layer` is the id it will get).
    pub(crate) fn record_adjust(&mut self, adjustment: &Adjustment, layer: LayerId) {
        if self.actions.mute > 0 || !self.actions.recording() {
            return;
        }
        self.record_step(Step::Adjust {
            adjustment: adjustment.clone(),
        });
        if let Some(rec) = self.actions.recording.as_mut() {
            rec.adjust = Some(layer);
        }
    }

    /// A registry action is about to run. Records it when it is a
    /// recordable edit; returns true when it did (the caller then calls
    /// [`App::record_menu_done`] once the action has run).
    pub(crate) fn record_menu(&mut self, id: &str) -> bool {
        if self.actions.mute > 0 || !self.actions.recording() || !actions::menu_recordable(id) {
            return false;
        }
        self.record_step(Step::Menu { id: id.to_string() });
        self.actions.mute += 1;
        true
    }

    pub(crate) fn record_menu_done(&mut self) {
        self.actions.mute = self.actions.mute.saturating_sub(1);
        self.actions_sync();
    }

    /// A dialog was confirmed (called before it applies).
    pub(crate) fn record_dialog(&mut self, d: &Dialog) {
        if !self.actions.recording() {
            return;
        }
        let step = match d {
            Dialog::Filter(f) => Step::Filter { filter: f.clone() },
            Dialog::ImageSize(w, h, _) => Step::ImageSize {
                width: *w,
                height: *h,
            },
            Dialog::CanvasSize(w, h, anchor) => Step::CanvasSize {
                width: *w,
                height: *h,
                anchor: [anchor.0, anchor.1],
            },
            Dialog::RotateBy(degrees, clockwise) => Step::RotateCanvas {
                degrees: if *clockwise { *degrees } else { -*degrees },
            },
            Dialog::Trim(transparent) => Step::Trim {
                transparent: *transparent,
            },
            _ => return,
        };
        self.record_step(step);
    }

    /// Account for history entries since the last look: those a recorded
    /// step claimed belong to it; the rest become "not recordable" notes.
    pub(crate) fn actions_sync(&mut self) {
        let Some(rec) = self.actions.recording.as_mut() else {
            return;
        };
        let history = self.editor.history();
        let n = history.len();
        if rec.doc_key != self.doc_key {
            // Another tab: recording follows the live document.
            rec.doc_key = self.doc_key;
            rec.seen = n;
            rec.claim = false;
            rec.adjust = None;
            return;
        }
        let mut notes = Vec::new();
        if n > rec.seen && !rec.claim {
            for label in &history[rec.seen..n] {
                // Properties tweaks to the adjustment just recorded are
                // picked up from the layer below, not noted.
                if rec.adjust.is_some() && label.starts_with("Edit ") {
                    continue;
                }
                notes.push(Step::Skipped {
                    what: label.to_string(),
                });
            }
        }
        rec.seen = n;
        rec.claim = false;
        if !notes.is_empty() {
            rec.adjust = None;
        }
        let refreshed = rec
            .adjust
            .and_then(|id| match &self.editor.doc().layer(id)?.content {
                lumenply_doc::LayerContent::Adjustment(a) => Some(a.clone()),
                _ => None,
            });
        if rec.adjust.is_some() && refreshed.is_none() {
            rec.adjust = None; // the layer is gone (undone or deleted)
        }
        let target = rec.target;
        let Some(entry) = self.actions.list.get_mut(target) else {
            return;
        };
        if let (Some(adj), Some(Step::Adjust { adjustment })) = (refreshed, entry.action.steps.last_mut()) {
            *adjustment = adj;
        }
        entry.action.steps.extend(notes);
    }

    // ---- panel commands ------------------------------------------------------

    /// Record into the selected user action, or a new one.
    pub(crate) fn start_recording(&mut self) {
        if self.actions.recording() {
            return;
        }
        let target = match self.actions.selected {
            Some(i) if self.actions.list.get(i).is_some_and(|e| !e.action.builtin) => i,
            _ => {
                let name = self.actions.unique_name("Action");
                self.actions.list.push(Entry {
                    action: Action::new(name),
                    open: true,
                });
                self.actions.list.len() - 1
            }
        };
        self.actions.selected = Some(target);
        self.actions.list[target].open = true;
        self.actions.recording = Some(Recorder {
            target,
            seen: self.editor.history().len(),
            doc_key: self.doc_key,
            claim: false,
            adjust: None,
        });
        self.actions.confirm_delete = None;
        let name = self.actions.list[target].action.name.clone();
        self.actions.message = Some((false, format!("Recording \u{201C}{name}\u{201D}")));
        self.status = format!("Recording action \u{201C}{name}\u{201D}: edits are added as steps");
    }

    pub(crate) fn stop_recording(&mut self) {
        self.actions_sync();
        let Some(rec) = self.actions.recording.take() else {
            return;
        };
        let Some(e) = self.actions.list.get(rec.target) else {
            return;
        };
        let skipped = e.action.steps.len() - e.action.playable();
        let mut msg = format!(
            "Recorded \u{201C}{}\u{201D}: {} step{}",
            e.action.name,
            e.action.playable(),
            if e.action.playable() == 1 { "" } else { "s" }
        );
        if skipped > 0 {
            msg += &format!(", {skipped} not recordable yet");
        }
        self.status = msg.clone();
        self.actions.message = Some((false, msg));
        self.actions.save();
    }

    /// Play action `i` on the live document as one undo step.
    pub(crate) fn play_action(&mut self, i: usize) {
        let Some(action) = self.actions.list.get(i).map(|e| e.action.clone()) else {
            return;
        };
        if self.no_doc {
            self.actions.message = Some((true, "Open a document first".into()));
            return;
        }
        if self.actions.recording_into() == Some(i) {
            self.actions.message = Some((true, "Stop recording before playing this action".into()));
            return;
        }
        if action.playable() == 0 {
            self.actions.message = Some((true, format!("\u{201C}{}\u{201D} has no steps", action.name)));
            return;
        }
        // Played while recording another action: its steps join that one.
        self.actions_sync();
        let size = (self.editor.doc().width, self.editor.doc().height);
        self.actions.mute += 1;
        let result = actions::play(&mut AppHost { app: self }, &action);
        self.actions.mute -= 1;
        self.below = lumenply_render::BelowCache::new();
        self.mark(None);
        self.fix_active();
        if (self.editor.doc().width, self.editor.doc().height) != size {
            self.view_cmd = Some(ViewCmd::Fit);
        }
        let msg = match result {
            Ok(n) => {
                if let Some(rec) = self.actions.recording.as_mut() {
                    rec.claim = true;
                    let t = rec.target;
                    let played = action.steps.iter().filter(|s| !matches!(s, Step::Skipped { .. }));
                    self.actions.list[t].action.steps.extend(played.cloned());
                }
                self.actions_sync();
                (
                    false,
                    format!(
                        "Played \u{201C}{}\u{201D}: {n} step{} (one undo step)",
                        action.name,
                        if n == 1 { "" } else { "s" }
                    ),
                )
            }
            Err(e) => (true, format!("\u{201C}{}\u{201D} stopped: {e}", action.name)),
        };
        self.status = msg.1.clone();
        self.actions.message = Some(msg);
    }

    fn delete_action(&mut self, i: usize) {
        if self.actions.list.get(i).is_none_or(|e| e.action.builtin) || self.actions.recording() {
            return;
        }
        let gone = self.actions.list.remove(i);
        self.actions.selected = None;
        self.actions.confirm_delete = None;
        self.actions.message = Some((false, format!("Deleted \u{201C}{}\u{201D}", gone.action.name)));
        self.actions.save();
    }

    fn rename_action(&mut self, i: usize, name: &str) {
        let name = name.trim();
        if name.is_empty() || self.actions.list.get(i).is_none_or(|e| e.action.builtin) {
            return;
        }
        if self.actions.find(name).is_some_and(|j| j != i) {
            self.actions.message = Some((true, format!("An action named \u{201C}{name}\u{201D} exists")));
            return;
        }
        self.actions.list[i].action.name = name.to_string();
        self.actions.save();
    }

    // ---- registry ----------------------------------------------------------------

    /// Run one of the Actions panel's registry actions; false for others.
    pub(crate) fn run_actions_panel_action(&mut self, id: &str) -> bool {
        match id {
            ACTIONS_PANEL => self.actions.shown = !self.actions.shown,
            ACTION_RECORD => {
                self.actions.shown = true;
                self.start_recording();
            }
            ACTION_STOP => self.stop_recording(),
            ACTION_PLAY => {
                if let Some(i) = self.actions.selected {
                    self.play_action(i);
                }
            }
            _ => return false,
        }
        true
    }

    /// Why an Actions registry action can't run (see [`App::action_block`]).
    pub(crate) fn actions_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        let a = &self.actions;
        Some(match id {
            ACTIONS_PANEL => None,
            ACTION_RECORD if a.recording() => Some("Already recording"),
            ACTION_RECORD => None,
            ACTION_STOP if !a.recording() => Some("Not recording"),
            ACTION_STOP => None,
            ACTION_PLAY if a.selected.is_none() => Some("Select an action in the Actions panel"),
            ACTION_PLAY if a.recording_into() == a.selected => Some("Stop recording first"),
            ACTION_PLAY => None,
            _ => return None,
        })
    }

    // ---- the panel ---------------------------------------------------------------

    /// Window ▸ Actions, floating at the canvas's top-left. Also keeps the
    /// recorder in step with the history every frame, shown or not.
    pub(crate) fn actions_panel_ui(&mut self, ctx: &egui::Context) {
        self.actions_sync();
        if !self.actions.shown || self.no_doc {
            return;
        }
        let Some(canvas) = self.panels.canvas_rect else {
            return;
        };
        let ruler = if self.prefs.show_rulers {
            guides::RULER
        } else {
            0.0
        };
        let at = egui::pos2(canvas.min.x + 12.0 + ruler, canvas.min.y + 12.0 + ruler);
        // Header, buttons and foot take ~180 px; stay clear of the canvas's
        // bottom bar (~60 px) on short windows.
        let extra = if self.actions.confirm_delete.is_some() {
            60.0
        } else {
            0.0
        };
        let max_list_h = (canvas.height() - 260.0 - extra).clamp(60.0, 420.0);
        let mut cmd: Option<PanelCmd> = None;
        egui::Area::new("actions-panel".into())
            .order(egui::Order::Middle)
            .sense(BACKDROP_SENSE)
            .fixed_pos(at)
            .show(ctx, |ui| {
                float_frame().show(ui, |ui| {
                    ui.set_width(PANEL_W);
                    ui.horizontal(|ui| {
                        if float_header(ui, "ACTIONS", "Close Actions") {
                            cmd = Some(PanelCmd::Close);
                        }
                    });
                    if self.actions.recording() {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.5, DANGER);
                            ui.label(RichText::new("Recording: your edits become steps").color(DANGER));
                        });
                    }
                    self.actions_buttons(ui, &mut cmd);
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("actions-list")
                        .max_height(max_list_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| self.actions_list(ui, &mut cmd));
                    if let Some(i) = self.actions.confirm_delete {
                        let name = self.actions.list.get(i).map_or("", |e| e.action.name.as_str());
                        ui.separator();
                        ui.label(RichText::new(format!("Delete \u{201C}{name}\u{201D}?")).color(TEXT));
                        ui.horizontal(|ui| {
                            if ui.button("Delete").clicked() {
                                cmd = Some(PanelCmd::Delete(i));
                            }
                            if ui.button("Cancel").clicked() {
                                cmd = Some(PanelCmd::CancelDelete);
                            }
                        });
                    }
                    if let Some((err, text)) = &self.actions.message {
                        ui.separator();
                        let c = if *err { DANGER } else { MUTED };
                        ui.add(egui::Label::new(RichText::new(text).small().color(c)).wrap());
                    }
                    ui.label(
                        RichText::new("Plays as one undo step. Steps act on the active layer.")
                            .small()
                            .color(MUTED),
                    );
                });
            });
        if let Some(c) = cmd {
            self.run_panel_cmd(c);
        }
    }

    fn actions_buttons(&mut self, ui: &mut egui::Ui, cmd: &mut Option<PanelCmd>) {
        let sel = self.actions.selected;
        let user_sel = sel.filter(|i| self.actions.list.get(*i).is_some_and(|e| !e.action.builtin));
        let recording = self.actions.recording();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
            if recording {
                if ui
                    .button(RichText::new("Stop").color(DANGER))
                    .on_hover_text("Stop recording")
                    .clicked()
                {
                    *cmd = Some(PanelCmd::Stop);
                }
            } else {
                let tip = if user_sel.is_some() {
                    "Record more steps into the selected action"
                } else {
                    "Record a new action"
                };
                if ui.button("Record").on_hover_text(tip).clicked() {
                    *cmd = Some(PanelCmd::Record);
                }
            }
            let can_play = sel.is_some() && self.actions.recording_into() != sel && !self.no_doc;
            if ui
                .add_enabled(can_play, egui::Button::new("Play"))
                .on_hover_text("Play the selected action on this document")
                .on_disabled_hover_text("Select an action to play")
                .clicked()
            {
                *cmd = sel.map(PanelCmd::Play);
            }
            if ui
                .add_enabled(!recording, egui::Button::new("New"))
                .on_hover_text("New action: starts recording")
                .clicked()
            {
                *cmd = Some(PanelCmd::New);
            }
            if ui
                .add_enabled(user_sel.is_some(), egui::Button::new("Rename"))
                .on_disabled_hover_text("Built-in actions keep their names")
                .clicked()
            {
                *cmd = user_sel.map(PanelCmd::BeginRename);
            }
            if ui
                .add_enabled(user_sel.is_some() && !recording, egui::Button::new("Delete"))
                .on_disabled_hover_text("Select one of your actions")
                .clicked()
            {
                *cmd = user_sel.map(PanelCmd::AskDelete);
            }
        });
    }

    fn actions_list(&mut self, ui: &mut egui::Ui, cmd: &mut Option<PanelCmd>) {
        let recording_into = self.actions.recording_into();
        for i in 0..self.actions.list.len() {
            let e = &self.actions.list[i];
            let (name, open, builtin) = (e.action.name.clone(), e.open, e.action.builtin);
            ui.horizontal(|ui| {
                // A painted disclosure triangle (the font has no ▸).
                let (rect, arrow) = ui.allocate_exact_size(egui::vec2(16.0, 18.0), Sense::click());
                let verb = if open { "Hide" } else { "Show" };
                let spoken = format!("{verb} the steps of {name}");
                arrow.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &spoken));
                let c = rect.center();
                let col = if arrow.hovered() { TEXT } else { MUTED };
                let pts = if open {
                    vec![
                        c + egui::vec2(-4.0, -2.0),
                        c + egui::vec2(4.0, -2.0),
                        c + egui::vec2(0.0, 3.0),
                    ]
                } else {
                    vec![
                        c + egui::vec2(-2.0, -4.0),
                        c + egui::vec2(3.0, 0.0),
                        c + egui::vec2(-2.0, 4.0),
                    ]
                };
                ui.painter()
                    .add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
                theme::focus_ring(ui, &arrow, rect, 3.0);
                if arrow.on_hover_text(format!("{verb} steps")).clicked() {
                    *cmd = Some(PanelCmd::Toggle(i));
                }
                match &mut self.actions.renaming {
                    Some((r, text)) if *r == i => {
                        let resp = ui.add(
                            egui::TextEdit::singleline(text)
                                .id_salt(("action-rename", i))
                                .desired_width(150.0),
                        );
                        theme::a11y_name(&resp, "Action name");
                        if !resp.has_focus() && !resp.lost_focus() {
                            resp.request_focus();
                        }
                        if resp.lost_focus() {
                            *cmd = Some(if ui.input(|i| i.key_pressed(Key::Escape)) {
                                PanelCmd::CancelRename
                            } else {
                                PanelCmd::Rename(i, text.clone())
                            });
                        }
                    }
                    _ => {
                        let sel = self.actions.selected == Some(i);
                        let resp = ui.selectable_label(sel, &name);
                        if resp.double_clicked() && !builtin {
                            *cmd = Some(PanelCmd::BeginRename(i));
                        } else if resp.clicked() {
                            *cmd = Some(PanelCmd::Select(i));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if recording_into == Some(i) {
                        ui.label(RichText::new("recording").small().color(DANGER));
                    } else if builtin {
                        ui.label(RichText::new("built-in").small().color(MUTED));
                    }
                });
            });
            if open {
                let steps = &self.actions.list[i].action.steps;
                if steps.is_empty() {
                    ui.label(RichText::new("      No steps yet").small().color(MUTED));
                }
                let mut n = 0;
                for step in steps {
                    let text = match step {
                        Step::Skipped { .. } => RichText::new(format!("      {}", step.describe()))
                            .small()
                            .italics()
                            .color(MUTED),
                        _ => {
                            n += 1;
                            RichText::new(format!("   {n:>2}. {}", step.describe()))
                                .small()
                                .color(TEXT)
                        }
                    };
                    ui.label(text);
                }
            }
        }
    }

    fn run_panel_cmd(&mut self, c: PanelCmd) {
        match c {
            PanelCmd::Close => self.actions.shown = false,
            PanelCmd::Record => self.start_recording(),
            PanelCmd::Stop => self.stop_recording(),
            PanelCmd::Play(i) => self.play_action(i),
            PanelCmd::New => {
                self.actions.selected = None;
                self.start_recording();
            }
            PanelCmd::Select(i) => {
                self.actions.selected = Some(i);
                self.actions.confirm_delete = None;
            }
            PanelCmd::Toggle(i) => {
                if let Some(e) = self.actions.list.get_mut(i) {
                    e.open = !e.open;
                }
            }
            PanelCmd::BeginRename(i) => {
                let name = self.actions.list.get(i).map(|e| e.action.name.clone());
                self.actions.renaming = name.map(|n| (i, n));
            }
            PanelCmd::Rename(i, name) => {
                self.actions.renaming = None;
                self.rename_action(i, &name);
            }
            PanelCmd::CancelRename => self.actions.renaming = None,
            PanelCmd::AskDelete(i) => self.actions.confirm_delete = Some(i),
            PanelCmd::CancelDelete => self.actions.confirm_delete = None,
            PanelCmd::Delete(i) => self.delete_action(i),
        }
    }

    // ---- debug tokens ------------------------------------------------------------

    /// `--screenshot-do` tokens (`actions:...`): `actions:show`,
    /// `actions:record` (shows the panel too), `actions:stop`,
    /// `actions:play=NAME`, `actions:open=NAME` (expand its steps),
    /// `actions:select=NAME`.
    pub(crate) fn debug_actions(&mut self, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("actions:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let found = self.actions.find(arg);
        match verb {
            "show" => self.actions.shown = true,
            "record" => {
                self.actions.shown = true;
                self.start_recording();
            }
            "stop" => self.stop_recording(),
            "play" => {
                if let Some(i) = found {
                    self.actions.selected = Some(i);
                    self.play_action(i);
                }
            }
            "open" => {
                if let Some(i) = found {
                    self.actions.list[i].open = true;
                }
            }
            "select" => self.actions.selected = found,
            _ => return false,
        }
        true
    }
}

/// What a click in the panel asked for, run after the panel is drawn.
enum PanelCmd {
    Close,
    Record,
    Stop,
    Play(usize),
    New,
    Select(usize),
    Toggle(usize),
    BeginRename(usize),
    Rename(usize, String),
    CancelRename,
    AskDelete(usize),
    CancelDelete,
    Delete(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y_tests::{ctx, launch, nameless};

    /// The app on a small blank document, with actions saved to a scratch
    /// file of the test's own.
    fn app_with(tag: &str) -> App {
        let mut app = launch(&[]);
        app.open_in_new_tab(blank(48, 32), None);
        let path = std::env::temp_dir().join(format!("lumenply-actions-{tag}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        app.actions = ActionsState::load_from(Some(path));
        app
    }

    #[test]
    fn recording_registry_and_dialog_edits_makes_steps_and_notes_the_rest() {
        let mut app = app_with("record");
        app.run_menu_action(ACTION_RECORD);
        assert!(app.actions.recording() && app.actions.shown);
        app.run_menu_action("img-flip-h");
        app.add_adjustment(Adjustment::Invert);
        // A Properties tweak right after lands in the Adjust step.
        let layer = app.active.unwrap();
        app.run(&SetAdjustment {
            layer,
            adjustment: Adjustment::Posterize { levels: 4 },
        });
        app.actions_sync();
        // A dialog confirmation, as dialogs() records it.
        let d = Dialog::ImageSize(24, 16, true);
        app.record_dialog(&d);
        app.run(&ResizeImage {
            width: 24,
            height: 16,
        });
        // The next frame accounts for it (a claim lasts until a sync).
        app.actions_sync();
        // An edit nothing records: a note.
        app.run(&AddPixelLayer::new("Paint here"));
        app.run_menu_action(ACTION_STOP);
        let a = &app.actions.list.last().unwrap().action;
        assert_eq!(a.name, "Action 1");
        assert_eq!(
            a.steps,
            vec![
                Step::Menu {
                    id: "img-flip-h".into()
                },
                Step::Adjust {
                    adjustment: Adjustment::Posterize { levels: 4 }
                },
                Step::ImageSize {
                    width: 24,
                    height: 16
                },
                Step::Skipped {
                    what: "Add layer 'Paint here'".into()
                },
            ]
        );
        // Saved: a fresh load finds it after the three built-ins.
        let again = ActionsState::load_from(app.actions.path.clone());
        assert_eq!(again.list.len(), 4);
        assert_eq!(again.list[3].action.steps, a.steps);
        let _ = std::fs::remove_file(app.actions.path.as_ref().unwrap());
    }

    #[test]
    fn auto_color_records_once_as_a_menu_step() {
        let mut app = app_with("auto");
        app.start_recording();
        app.last_flat = Some(lumenply_render::composite_raster(app.editor.doc()));
        app.run_menu_action("auto-color");
        app.stop_recording();
        let steps = &app.actions.list.last().unwrap().action.steps;
        assert_eq!(
            steps,
            &vec![Step::Menu {
                id: "auto-color".into()
            }]
        );
        let _ = std::fs::remove_file(app.actions.path.as_ref().unwrap());
    }

    #[test]
    fn playing_is_one_undo_step_and_errors_name_the_step() {
        let mut app = app_with("play");
        let before = app.editor.history().len();
        let i = app.actions.find("Black & white contrast").unwrap();
        app.play_action(i);
        assert_eq!(app.editor.history().len(), before + 1);
        assert_eq!(
            app.editor.history().last().copied(),
            Some("Action: Black & white contrast")
        );
        assert_eq!(app.editor.doc().layers().len(), 3, "background + B&W + Curves");
        assert!(app.status.contains("2 steps"), "{}", app.status);
        // A step the registry blocks stops playback and rolls back.
        app.actions.list.push(Entry {
            action: Action {
                name: "Needs selection".into(),
                steps: vec![
                    Step::Menu { id: "rot-cw".into() },
                    Step::Menu { id: "crop".into() },
                ],
                builtin: false,
            },
            open: false,
        });
        let (w, h) = (app.editor.doc().width, app.editor.doc().height);
        app.run(&SetSelection { selection: None });
        let n = app.editor.history().len();
        app.play_action(app.actions.list.len() - 1);
        assert_eq!(app.editor.history().len(), n, "rolled back");
        assert_eq!((app.editor.doc().width, app.editor.doc().height), (w, h));
        let (err, msg) = app.actions.message.clone().unwrap();
        assert!(err && msg.contains("step 2 (Crop to selection)"), "{msg}");
    }

    #[test]
    fn the_actions_panel_names_every_control() {
        let mut app = app_with("a11y");
        app.actions.shown = true;
        app.actions.list[0].open = true;
        app.start_recording();
        app.run_menu_action("invert-sel");
        let ctx = ctx();
        assert_eq!(nameless(&mut app, &ctx), Vec::<String>::new());
        app.stop_recording();
        let last = app.actions.list.len() - 1;
        app.actions.renaming = Some((last, "Renamed".into()));
        app.actions.confirm_delete = Some(last);
        assert_eq!(nameless(&mut app, &ctx), Vec::<String>::new());
        let _ = std::fs::remove_file(app.actions.path.as_ref().unwrap());
    }

    #[test]
    fn every_recordable_step_is_a_registry_action() {
        // A typo in RECORDABLE_MENU would record steps nothing can play.
        for (id, _) in actions::RECORDABLE_MENU {
            let mut app = app_with("registry");
            app.status.clear();
            app.run_menu_action_unrecorded(id);
            assert!(!app.status.starts_with("Unknown command"), "{id}: {}", app.status);
        }
    }

    #[test]
    fn rename_and_delete_keep_builtins_and_names_unique() {
        let mut app = app_with("rename");
        app.start_recording();
        app.stop_recording();
        let last = app.actions.list.len() - 1;
        app.rename_action(last, "Vintage fade");
        assert_eq!(
            app.actions.list[last].action.name, "Action 1",
            "names stay unique"
        );
        app.rename_action(last, "  Mine ");
        assert_eq!(app.actions.list[last].action.name, "Mine");
        app.rename_action(0, "Hacked");
        assert_eq!(app.actions.list[0].action.name, "Web export prep");
        app.delete_action(0);
        assert_eq!(app.actions.list.len(), 4, "built-ins can't be deleted");
        app.delete_action(last);
        assert_eq!(app.actions.list.len(), 3);
        let _ = std::fs::remove_file(app.actions.path.as_ref().unwrap());
    }
}
