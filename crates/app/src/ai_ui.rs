//! Local AI selection and masking in the editor (ADR 0028): Object
//! Selection (a mode of the Wand tool, beside Quick Selection), Select ▸
//! Subject, Layer ▸ Remove Background and Preferences ▸ AI models, on top
//! of [`crate::ai::AiService`].
//!
//! - **Off the UI thread.** A request snapshots the document (cheap: tiles
//!   are shared) and runs on a worker ([`crate::ai_jobs`]); one inference
//!   runs at a time and later requests queue behind it, so Shift-clicks
//!   land in order. A progress card over the canvas says what is running
//!   and can cancel it.
//! - **Results are ordinary edits.** A finished result becomes one command
//!   (`SelectFromMatte`, `MaskFromMatte`) on the document it was asked
//!   for, even if another tab is in front by then; a closed document's
//!   result is dropped with a message. Results wait while the user is
//!   mid-gesture or a dialog holds a preview step.
//! - **First use asks.** A feature whose model isn't installed opens a
//!   dialog naming the model, its size, source and licence; the download
//!   runs only after Download, with progress and Cancel, and the request
//!   then runs by itself.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::ai::{format_bytes, AiPrompt, AiService, FakeAi, ModelInfoView, ModelKey, NoEngine};
use crate::ai_jobs::Job;
use lumenply_core::ai_masks::{MaskFromMatte, Matte, SelectFromMatte};

/// Select ▸ Subject.
pub(crate) const SELECT_SUBJECT: &str = "select-subject";
/// Layer ▸ Remove Background.
pub(crate) const REMOVE_BG: &str = "remove-bg";
/// The Wand tool in Object Selection mode.
pub(crate) const OBJECT_TOOL: &str = "tool-object-select";
/// Preferences on its AI models page.
pub(crate) const AI_MODELS: &str = "ai-models";

/// What an inference job reports it is doing.
const PHASE_RUN: u8 = 0;
/// Encoding the image for the first click on it (the slow part).
const PHASE_ENCODING: u8 = 1;

/// One AI edit the user asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AiTask {
    /// A click or box with the Object Selection tool, combined with the
    /// selection by `op` (Shift adds, Alt subtracts).
    Object { prompt: AiPrompt, op: CombineOp },
    /// Select ▸ Subject: replaces the selection.
    Subject,
    /// Layer ▸ Remove Background on this layer.
    RemoveBackground { layer: LayerId },
}

impl AiTask {
    pub(crate) fn model(&self) -> ModelKey {
        match self {
            AiTask::Object { .. } => ModelKey::MobileSam,
            AiTask::Subject | AiTask::RemoveBackground { .. } => ModelKey::BiRefNetLite,
        }
    }

    /// The feature's name, as menus and messages say it.
    pub(crate) fn feature(&self) -> &'static str {
        match self {
            AiTask::Object { .. } => "Object Selection",
            AiTask::Subject => "Select Subject",
            AiTask::RemoveBackground { .. } => "Remove Background",
        }
    }

    /// The history step the result becomes.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            AiTask::Object { .. } => "Object selection",
            AiTask::Subject => "Select subject",
            AiTask::RemoveBackground { .. } => "Remove background",
        }
    }

    /// The options bar's short form of [`AiTask::doing`].
    fn doing_short(&self, phase: u8) -> &'static str {
        match self {
            AiTask::Object { .. } if phase == PHASE_ENCODING => "Analysing the image…",
            _ => "Selecting…",
        }
    }

    /// What the progress card says while it runs.
    fn doing(&self, phase: u8) -> &'static str {
        match self {
            AiTask::Object { .. } if phase == PHASE_ENCODING => "Analysing the image (first click only)…",
            AiTask::Object { .. } => "Selecting the object…",
            AiTask::Subject => "Finding the subject…",
            AiTask::RemoveBackground { .. } => "Separating the subject from the background…",
        }
    }
}

/// Where a task's image comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    /// The composite (Select Subject; Object Selection with All layers).
    Merged,
    /// One layer's pixels on the canvas.
    Layer(LayerId),
}

/// A task with the document as it was when asked for.
pub(crate) struct AiRequest {
    task: AiTask,
    doc_key: u64,
    doc: Document,
    source: Source,
}

/// The inference job in flight.
struct Running {
    task: AiTask,
    doc_key: u64,
    size: (u32, u32),
    job: Job<Result<Vec<f32>, String>>,
}

/// A result waiting to be applied.
struct Finished {
    task: AiTask,
    doc_key: u64,
    size: (u32, u32),
    result: Result<Vec<f32>, String>,
    secs: f32,
}

/// The first-use dialog's state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConsentPhase {
    /// Name, size, source and licence; Download or Cancel.
    Ask,
    /// The download runs (its job is in `AiState::downloads`).
    Downloading,
    /// The download failed; why, with Try again.
    Failed(String),
}

/// The first-use dialog: the model it is about and what runs once it is
/// installed.
pub(crate) struct Consent {
    pub(crate) model: ModelKey,
    pending: Option<AiRequest>,
    pub(crate) phase: ConsentPhase,
}

/// Everything AI the app keeps between frames.
pub(crate) struct AiState {
    pub(crate) service: Arc<dyn AiService>,
    /// The models as last read from the service.
    models: Vec<ModelInfoView>,
    downloads: Vec<(ModelKey, Job<Result<(), String>>)>,
    running: Option<Running>,
    queue: VecDeque<AiRequest>,
    finished: VecDeque<Finished>,
    pub(crate) consent: Option<Consent>,
    /// Preferences shows its AI models page.
    pub(crate) prefs_tab: bool,
    /// Object Selection's box drag in screen points: (start, now).
    drag: Option<(Pos2, Pos2)>,
    /// The UI's context, so workers can ask for a repaint.
    waker: Option<egui::Context>,
}

impl AiState {
    /// Without the engine in this build, the features say so (see
    /// [`NoEngine`]); the engine adapter replaces this service.
    pub(crate) fn new() -> AiState {
        AiState::with(Arc::new(NoEngine))
    }

    pub(crate) fn with(service: Arc<dyn AiService>) -> AiState {
        AiState {
            models: service.models(),
            service,
            downloads: Vec::new(),
            running: None,
            queue: VecDeque::new(),
            finished: VecDeque::new(),
            consent: None,
            prefs_tab: false,
            drag: None,
            waker: None,
        }
    }

    fn info(&self, model: ModelKey) -> ModelInfoView {
        self.models
            .iter()
            .find(|m| m.key == model)
            .cloned()
            .or_else(|| crate::ai::catalogue().into_iter().find(|m| m.key == model))
            .expect("every model is catalogued")
    }

    pub(crate) fn installed(&self, model: ModelKey) -> bool {
        self.models.iter().any(|m| m.key == model && m.installed)
    }

    fn downloading(&self, model: ModelKey) -> Option<&Job<Result<(), String>>> {
        self.downloads.iter().find(|(m, _)| *m == model).map(|(_, j)| j)
    }

    /// Anything running, queued or waiting to be applied.
    pub(crate) fn active(&self) -> bool {
        !self.downloads.is_empty()
            || self.running.is_some()
            || !self.queue.is_empty()
            || !self.finished.is_empty()
    }
}

/// The task's image: the composite or one layer's pixels, canvas-sized.
fn source_raster(doc: &Document, source: Source) -> Result<Raster, String> {
    match source {
        Source::Merged => Ok(lumenply_render::composite_raster(doc)),
        Source::Layer(id) => {
            let l = doc.layer(id).ok_or("the layer was deleted")?;
            let store = l.raster_store().ok_or("the layer has no pixels")?;
            Ok(store.to_raster(doc.canvas()))
        }
    }
}

impl App {
    /// Swap the AI service (the engine adapter at startup, the fake in
    /// tests and screenshot runs).
    pub(crate) fn ai_use_service(&mut self, service: Arc<dyn AiService>) {
        let waker = self.ai.waker.take();
        self.ai = AiState::with(service);
        self.ai.waker = waker;
    }

    fn ai_refresh_models(&mut self) {
        self.ai.models = self.ai.service.models();
    }

    /// Ask for `task` on the live document. It runs at once (queued behind
    /// a running one) when its model is installed; otherwise the first-use
    /// dialog asks to download the model and runs it afterwards.
    pub(crate) fn ai_request(&mut self, task: AiTask) {
        if self.no_doc {
            return;
        }
        if let Some(why) = self.ai.service.unavailable() {
            self.status = format!("{}: {why}", task.feature());
            return;
        }
        let source = match task {
            AiTask::Subject => Source::Merged,
            AiTask::RemoveBackground { layer } => Source::Layer(layer),
            AiTask::Object { .. } => match (self.sample_merged, self.active, self.active_is_pixel()) {
                (false, Some(layer), true) => Source::Layer(layer),
                _ => Source::Merged,
            },
        };
        let req = AiRequest {
            task,
            doc_key: self.doc_key,
            doc: self.editor.doc().clone(),
            source,
        };
        if !self.ai.installed(task.model()) {
            // One question at a time; a click while it is up is dropped.
            if self.ai.consent.is_none() {
                self.ai.consent = Some(Consent {
                    model: task.model(),
                    pending: Some(req),
                    phase: ConsentPhase::Ask,
                });
            }
            return;
        }
        self.ai.queue.push_back(req);
        self.ai_start_next();
    }

    /// Start the next queued inference unless one is running.
    fn ai_start_next(&mut self) {
        if self.ai.running.is_some() {
            return;
        }
        let Some(req) = self.ai.queue.pop_front() else {
            return;
        };
        let service = self.ai.service.clone();
        let (task, doc_key) = (req.task, req.doc_key);
        let size = (req.doc.width, req.doc.height);
        let job = Job::spawn(self.ai.waker.clone(), move |progress, _| {
            let image = source_raster(&req.doc, req.source)?;
            match req.task {
                AiTask::Object { prompt, .. } => {
                    progress.set_phase(PHASE_RUN);
                    service.select(&image, &[prompt], &|| progress.set_phase(PHASE_ENCODING))
                }
                AiTask::Subject | AiTask::RemoveBackground { .. } => service.matte(&image),
            }
        });
        self.ai.running = Some(Running {
            task,
            doc_key,
            size,
            job,
        });
    }

    /// Download a model in the background (Preferences, or the first-use
    /// dialog's Download).
    pub(crate) fn ai_start_download(&mut self, model: ModelKey) {
        if self.ai.downloading(model).is_some() {
            return;
        }
        let service = self.ai.service.clone();
        let job = Job::spawn(self.ai.waker.clone(), move |p, cancel| {
            service.download(model, &|done, total| p.set(done, total), cancel)
        });
        self.ai.downloads.push((model, job));
    }

    pub(crate) fn ai_cancel_download(&mut self, model: ModelKey) {
        if let Some(job) = self.ai.downloading(model) {
            job.cancel();
        }
    }

    /// The first-use dialog's Download (and Try again).
    pub(crate) fn ai_consent_download(&mut self) {
        let Some(model) = self.ai.consent.as_ref().map(|c| c.model) else {
            return;
        };
        self.ai_start_download(model);
        if let Some(c) = self.ai.consent.as_mut() {
            c.phase = ConsentPhase::Downloading;
        }
    }

    /// The first-use dialog's Cancel: stops a running download (nothing is
    /// installed) and drops the request that waited for it.
    pub(crate) fn ai_consent_cancel(&mut self) {
        if let Some(c) = self.ai.consent.take() {
            if c.phase == ConsentPhase::Downloading {
                self.ai_cancel_download(c.model);
            }
        }
    }

    /// Cancel the running inference and everything queued behind it. Work
    /// already inside the model finishes, and its result is dropped.
    pub(crate) fn ai_cancel_running(&mut self) {
        self.ai.queue.clear();
        if let Some(r) = &self.ai.running {
            r.job.cancel();
            self.status = format!("{} cancelled", r.task.feature());
        }
    }

    pub(crate) fn ai_remove_model(&mut self, model: ModelKey) {
        let name = self.ai.info(model).name;
        match self.ai.service.remove(model) {
            Ok(()) => self.status = format!("Removed {name}"),
            Err(e) => self.status = format!("Couldn't remove {name}: {e}"),
        }
        self.ai_refresh_models();
    }

    /// Something the user is in the middle of that an arriving result must
    /// not land under: a stroke or drag, a dialog with a preview step, a
    /// live transform, text being typed.
    fn ai_busy(&self, pointer_down: bool) -> bool {
        pointer_down
            || self.dialog.is_some()
            || self.adjx.is_some()
            || self.drag.is_some()
            || self.xform.is_some()
            || self.palette.is_some()
            || self.text_editing()
            || self.liquify.is_some()
            || self.puppet.is_some()
            || self.cas.is_some()
            || self.camera_raw.is_some()
            || self.select_mask.is_some()
            || self.export_as.is_some()
    }

    /// Collect finished jobs: a download updates the model list (and runs
    /// what waited for it); an inference result is applied as soon as
    /// nothing is in the way.
    pub(crate) fn ai_poll(&mut self, pointer_down: bool) {
        let mut i = 0;
        while i < self.ai.downloads.len() {
            match self.ai.downloads[i].1.poll() {
                None => i += 1,
                Some(out) => {
                    let (model, job) = self.ai.downloads.remove(i);
                    self.ai_download_finished(model, job.cancelled(), out.and_then(|r| r));
                }
            }
        }
        if let Some(out) = self.ai.running.as_ref().and_then(|r| r.job.poll()) {
            let r = self.ai.running.take().expect("checked above");
            if !r.job.cancelled() {
                self.ai.finished.push_back(Finished {
                    task: r.task,
                    doc_key: r.doc_key,
                    size: r.size,
                    result: out.and_then(|x| x),
                    secs: r.job.started.elapsed().as_secs_f32(),
                });
            }
            self.ai_start_next();
        }
        if !self.ai_busy(pointer_down) {
            while let Some(f) = self.ai.finished.pop_front() {
                self.ai_apply(f);
            }
        }
    }

    fn ai_download_finished(&mut self, model: ModelKey, cancelled: bool, result: Result<(), String>) {
        self.ai_refresh_models();
        let info = self.ai.info(model);
        let ours = self.ai.consent.as_ref().is_some_and(|c| c.model == model);
        match result {
            Ok(()) => {
                self.status = format!("Installed {} ({})", info.name, format_bytes(info.bytes));
                if ours {
                    let c = self.ai.consent.take().expect("checked above");
                    if let (Some(req), false) = (c.pending, cancelled) {
                        self.ai.queue.push_back(req);
                        self.ai_start_next();
                    }
                }
            }
            Err(_) if cancelled => {
                self.status = format!("Download of {} cancelled; nothing was installed", info.name);
                if ours {
                    self.ai.consent = None;
                }
            }
            Err(e) => {
                self.status = format!("Couldn't download {}: {e}", info.name);
                if let (true, Some(c)) = (ours, self.ai.consent.as_mut()) {
                    c.phase = ConsentPhase::Failed(e);
                }
            }
        }
    }

    /// Turn a finished result into its command on the document it was
    /// asked for: the live one, or a parked tab's (where it becomes that
    /// document's next undo step). A closed document's result is dropped.
    fn ai_apply(&mut self, f: Finished) {
        let feature = f.task.feature();
        let alpha = match f.result {
            Ok(a) => a,
            Err(e) => {
                self.status = format!("{feature} failed: {e}");
                return;
            }
        };
        let Some(matte) = Matte::new(f.size.0, f.size.1, alpha) else {
            self.status = format!("{feature} failed: the model's result doesn't match the image size");
            return;
        };
        let cmd: Box<dyn Command> = match f.task {
            AiTask::Object { op, .. } => {
                if matte.is_empty() && op != CombineOp::Intersect {
                    self.status = "Object Selection found nothing there".into();
                    return;
                }
                Box::new(SelectFromMatte {
                    matte,
                    op,
                    label: f.task.label().into(),
                })
            }
            AiTask::Subject => {
                if matte.is_empty() {
                    self.status = "Select Subject found no subject".into();
                    return;
                }
                Box::new(SelectFromMatte {
                    matte,
                    op: CombineOp::Replace,
                    label: f.task.label().into(),
                })
            }
            AiTask::RemoveBackground { layer } => {
                // An empty matte would hide the whole layer.
                if matte.is_empty() {
                    self.status = "Remove Background found no subject; the layer is unchanged".into();
                    return;
                }
                Box::new(MaskFromMatte { layer, matte })
            }
        };
        if !self.no_doc && f.doc_key == self.doc_key {
            // `run` reports a failure in the status bar.
            self.status.clear();
            self.run(cmd.as_ref());
            if self.status.is_empty() {
                self.status = format!("{} ({:.1} s)", f.task.label(), f.secs);
            }
        } else if let Some(tab) = self.tabs.iter_mut().find(|t| t.doc_key == f.doc_key) {
            let title = tab.title();
            self.status = match tab.editor.execute(cmd.as_ref()) {
                Ok(()) => format!("{feature} finished in {title} (another tab)"),
                Err(e) => format!("{feature} couldn't be applied to {title}: {e}"),
            };
        } else {
            self.status = format!("{feature} finished after its document was closed; the result was dropped");
        }
    }

    /// Block until every job has finished and its result is applied, or
    /// `timeout` passes (tests and `ai:wait` in screenshot runs).
    pub(crate) fn ai_wait(&mut self, timeout: Duration) {
        let t = Instant::now();
        loop {
            self.ai_poll(false);
            let waiting =
                !self.ai.downloads.is_empty() || self.ai.running.is_some() || !self.ai.queue.is_empty();
            if !waiting || t.elapsed() > timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    // ---- frame ---------------------------------------------------------------------

    /// Each frame: pick up finished work, then the first-use dialog and
    /// the progress card. Keeps frames coming while anything runs.
    pub(crate) fn ai_ui(&mut self, ctx: &egui::Context) {
        self.ai.waker = Some(ctx.clone());
        let down = ctx.input(|i| i.pointer.any_down());
        self.ai_poll(down);
        if !matches!(self.dialog, Some(Dialog::Preferences(..))) {
            self.ai.prefs_tab = false;
        }
        self.ai_consent_ui(ctx);
        self.ai_progress_card(ctx);
        if self.ai.active() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// The first-use dialog: what will be downloaded and that images stay
    /// local; then the download's progress; then, if it failed, why.
    fn ai_consent_ui(&mut self, ctx: &egui::Context) {
        let Some(c) = self.ai.consent.as_ref() else {
            return;
        };
        // Another modal dialog first.
        if self.dialog.is_some() {
            return;
        }
        let info = self.ai.info(c.model);
        let phase = c.phase.clone();
        let feature = c
            .pending
            .as_ref()
            .map_or(info.purpose.clone(), |r| r.task.feature().to_string());
        let progress = self.ai.downloading(c.model).map(|j| j.progress.get());
        let (enter, esc) = if ctx.wants_keyboard_input() {
            (false, false)
        } else {
            ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)))
        };
        let title = match phase {
            ConsentPhase::Ask => "Download an AI model",
            ConsentPhase::Downloading => "Downloading an AI model",
            ConsentPhase::Failed(_) => "Download failed",
        };
        crate::dialogs::modal_backdrop(ctx, true);
        #[derive(PartialEq)]
        enum Act {
            Download,
            Cancel,
        }
        let mut act = None;
        let shown = egui::Window::new(title)
            .id(egui::Id::new("ai-consent"))
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| {
                crate::dialogs::titled(ui, title, |ui| {
                    raise_controls(ui);
                    ui.set_min_width(360.0);
                    ui.set_max_width(424.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    match &phase {
                        ConsentPhase::Ask => {
                            crate::dialogs::note(
                                ui,
                                &format!(
                                    "{feature} needs the {} model, which isn't installed yet.",
                                    info.name
                                ),
                            );
                            model_facts(ui, &info);
                            crate::dialogs::note(
                                ui,
                                "It is downloaded once, checked and kept in the Lumenply data folder; \
                                 after that it works offline. Your images never leave this computer: \
                                 the model runs on it.",
                            );
                            crate::dialogs::footer(ui, |ui| {
                                if ui.add(primary_button("Download")).clicked() || enter {
                                    act = Some(Act::Download);
                                }
                                if ui.add(footer_button("Cancel")).clicked() || esc {
                                    act = Some(Act::Cancel);
                                }
                            });
                        }
                        ConsentPhase::Downloading => {
                            let (done, total) = progress.unwrap_or((0, info.bytes));
                            let total = total.max(1);
                            ui.label(RichText::new(format!("{} ({})", info.name, info.purpose)).color(TEXT));
                            let frac = done as f32 / total as f32;
                            progress_bar(ui, frac, 400.0, "Download progress");
                            ui.label(
                                RichText::new(format!(
                                    "{} of {}  ·  {:.0}%",
                                    format_bytes(done),
                                    format_bytes(total),
                                    frac * 100.0
                                ))
                                .monospace()
                                .color(MUTED),
                            );
                            crate::dialogs::note(
                                ui,
                                &format!(
                                    "From {}. {feature} runs as soon as it is installed. \
                                     Your images stay on this computer.",
                                    info.source
                                ),
                            );
                            crate::dialogs::footer(ui, |ui| {
                                if ui.add(footer_button("Cancel")).clicked() || esc {
                                    act = Some(Act::Cancel);
                                }
                            });
                        }
                        ConsentPhase::Failed(why) => {
                            ui.label(RichText::new(format!("{} wasn't installed.", info.name)).color(TEXT));
                            ui.add(egui::Label::new(RichText::new(why).color(DANGER)).wrap());
                            crate::dialogs::note(
                                ui,
                                "Check the internet connection and try again. The model is needed \
                                 only once; after that, everything works offline.",
                            );
                            crate::dialogs::footer(ui, |ui| {
                                if ui.add(primary_button("Try again")).clicked() || enter {
                                    act = Some(Act::Download);
                                }
                                if ui.add(footer_button("Cancel")).clicked() || esc {
                                    act = Some(Act::Cancel);
                                }
                            });
                        }
                    }
                })
            });
        if let Some(shown) = shown {
            ctx.move_to_top(shown.response.layer_id);
            ctx.accesskit_node_builder(shown.response.id, |b| {
                b.set_role(egui::accesskit::Role::Dialog);
                b.set_name(title);
            });
        }
        match act {
            Some(Act::Download) => self.ai_consent_download(),
            Some(Act::Cancel) => self.ai_consent_cancel(),
            None => {}
        }
    }

    /// A small card at the top of the canvas (the bottom holds the zoom
    /// and selection bars) while an inference, or a download started from
    /// Preferences, runs; with Cancel.
    fn ai_progress_card(&mut self, ctx: &egui::Context) {
        // A modal dialog (Preferences shows downloads in its own rows) or
        // the first-use dialog is in front: never above it.
        if self.no_doc || self.dialog.is_some() || self.ai.consent.is_some() {
            return;
        }
        // The running inference, once it has taken long enough to notice
        // (a cached Object Selection click is over in milliseconds).
        let infer = self
            .ai
            .running
            .as_ref()
            .filter(|r| !r.job.cancelled())
            .and_then(|r| {
                let phase = r.job.progress.phase();
                let slow = phase == PHASE_ENCODING
                    || !matches!(r.task, AiTask::Object { .. })
                    || r.job.started.elapsed() > Duration::from_millis(250);
                slow.then(|| (r.task.doing(phase).to_string(), self.ai.queue.len()))
            });
        let downloads: Vec<(ModelKey, String, f32)> = self
            .ai
            .downloads
            .iter()
            .filter(|(_, j)| !j.cancelled())
            .map(|(m, j)| (*m, self.ai.info(*m).name, j.progress.fraction()))
            .collect();
        if infer.is_none() && downloads.is_empty() {
            return;
        }
        let area = self.panels.canvas_rect.unwrap_or(ctx.screen_rect());
        let mut cancel_infer = false;
        let mut cancel_download = None;
        egui::Area::new(egui::Id::new("ai-progress"))
            .order(egui::Order::Foreground)
            .sense(BACKDROP_SENSE)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(area.center().x, area.top() + 12.0))
            .show(ctx, |ui| {
                egui::Frame::window(ui.style())
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0, LINE))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        raise_controls(ui);
                        if let Some((text, waiting)) = &infer {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                                ui.label(RichText::new(text).color(TEXT));
                                if *waiting > 0 {
                                    ui.label(RichText::new(format!("+{waiting} waiting")).color(MUTED));
                                }
                                if ui
                                    .add(footer_button("Cancel"))
                                    .on_hover_text("Stop and drop the result")
                                    .clicked()
                                {
                                    cancel_infer = true;
                                }
                            });
                        }
                        for (model, name, frac) in &downloads {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("Downloading {name}")).color(TEXT));
                                progress_bar(ui, *frac, 160.0, &format!("{name} download progress"));
                                ui.label(
                                    RichText::new(format!("{:3.0}%", frac * 100.0))
                                        .monospace()
                                        .color(MUTED),
                                );
                                if ui.add(footer_button("Cancel")).clicked() {
                                    cancel_download = Some(*model);
                                }
                            });
                        }
                    });
            });
        if cancel_infer {
            self.ai_cancel_running();
        }
        if let Some(m) = cancel_download {
            self.ai_cancel_download(m);
        }
    }

    // ---- Preferences ▸ AI models -----------------------------------------------------

    /// The switch at the top of Preferences and, when "AI models" is
    /// chosen, that page. True when the AI page was drawn (the general
    /// settings are then left out).
    pub(crate) fn ai_prefs_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut ai = self.ai.prefs_tab;
        segmented(ui, &mut ai, &[(false, "General"), (true, "AI models")]);
        self.ai.prefs_tab = ai;
        if !ai {
            return false;
        }
        // Installed state can change outside the app; reread it while shown.
        self.ai_refresh_models();
        section_title(ui, "AI MODELS");
        crate::dialogs::note(
            ui,
            "Object Selection, Select Subject and Remove Background run on this computer \
             with these models, downloaded once on first use. Images never leave this computer.",
        );
        let unavailable = self.ai.service.unavailable();
        let busy = self.ai.running.is_some();
        let mut download = None;
        let mut cancel = None;
        let mut remove = None;
        for m in self.ai.models.clone() {
            ui.add_space(2.0);
            egui::Frame::none()
                .fill(GROUND)
                .stroke(Stroke::new(1.0, LINE))
                .rounding(RADIUS)
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&m.name).strong().color(TEXT));
                                let (state, ink) = if m.installed {
                                    ("Installed", ACCENT)
                                } else {
                                    ("Not installed", MUTED)
                                };
                                ui.label(RichText::new(state).small().color(ink));
                            });
                            ui.label(RichText::new(&m.purpose).color(MUTED));
                            ui.label(
                                RichText::new(format!("{}  ·  {} licence", format_bytes(m.bytes), m.licence))
                                    .color(MUTED),
                            );
                            ui.add(
                                egui::Label::new(RichText::new(&m.source).small().color(MUTED)).truncate(),
                            )
                            .on_hover_text(format!("Downloaded from {}", m.source));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(job) = self.ai.downloading(m.key) {
                                if ui.add(footer_button("Cancel")).clicked() {
                                    cancel = Some(m.key);
                                }
                                let frac = job.progress.fraction();
                                ui.label(
                                    RichText::new(format!("{:3.0}%", frac * 100.0))
                                        .monospace()
                                        .color(MUTED),
                                );
                                progress_bar(ui, frac, 90.0, &format!("{} download progress", m.name));
                            } else if m.installed {
                                let r = ui
                                    .add_enabled(!busy, footer_button("Remove"))
                                    .on_hover_text(format!("Delete {} from this computer", m.name))
                                    .on_disabled_hover_text("Wait for the running job to finish");
                                if r.clicked() {
                                    remove = Some(m.key);
                                }
                            } else {
                                let r = ui
                                    .add_enabled(unavailable.is_none(), footer_button("Download"))
                                    .on_hover_text(format!(
                                        "Download {} from {}",
                                        format_bytes(m.bytes),
                                        m.source
                                    ))
                                    .on_disabled_hover_text(unavailable.clone().unwrap_or_default());
                                if r.clicked() {
                                    download = Some(m.key);
                                }
                            }
                        });
                    });
                });
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            row_label(ui, "Runs on", LABEL_W);
            match &unavailable {
                Some(why) => ui.label(RichText::new(why).color(MUTED)),
                None => ui.label(RichText::new(self.ai.service.provider()).color(TEXT)),
            };
        });
        if let Some(dir) = session::data_dir() {
            ui.horizontal(|ui| {
                row_label(ui, "Kept in", LABEL_W);
                let path = dir.join("models").display().to_string();
                ui.add(egui::Label::new(RichText::new(&path).monospace().small().color(MUTED)).truncate())
                    .on_hover_text(path);
            });
        }
        if let Some(m) = download {
            self.ai_start_download(m);
        }
        if let Some(m) = cancel {
            self.ai_cancel_download(m);
        }
        if let Some(m) = remove {
            self.ai_remove_model(m);
        }
        true
    }

    // ---- Object Selection ------------------------------------------------------------

    /// Canvas input in Object Selection mode: a click selects the object
    /// under the pointer, a drag draws a box around one. Shift adds, Alt
    /// subtracts, both intersect (as with every selection tool).
    pub(crate) fn object_select_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let primary = egui::PointerButton::Primary;
        if resp.drag_started_by(primary) {
            // press_origin is gone once the button is released.
            let start = ctx
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos());
            self.ai.drag = start.map(|p| (p, p));
        }
        if resp.dragged_by(primary) {
            if let (Some(d), Some(p)) = (self.ai.drag.as_mut(), resp.interact_pointer_pos()) {
                d.1 = p;
            }
        }
        if resp.drag_stopped() {
            if let Some((a, b)) = self.ai.drag.take() {
                let (ax, ay) = to_doc(a);
                let (bx, by) = to_doc(b);
                // A drag shorter than a few points is a click.
                let prompt = if (a.x - b.x).abs() >= 4.0 && (a.y - b.y).abs() >= 4.0 {
                    AiPrompt::Box {
                        x0: ax.min(bx),
                        y0: ay.min(by),
                        x1: ax.max(bx),
                        y1: ay.max(by),
                    }
                } else {
                    AiPrompt::Point {
                        x: ax,
                        y: ay,
                        positive: true,
                    }
                };
                let op = self.selection_op(ctx);
                self.ai_request(AiTask::Object { prompt, op });
            }
        } else if resp.clicked_by(primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let (x, y) = to_doc(p);
                let op = self.selection_op(ctx);
                self.ai_request(AiTask::Object {
                    prompt: AiPrompt::Point { x, y, positive: true },
                    op,
                });
            }
        }
    }

    /// The box being dragged.
    pub(crate) fn paint_object_select(&self, painter: &egui::Painter) {
        if let Some((a, b)) = self.ai.drag {
            let r = egui::Rect::from_two_pos(a, b);
            painter.rect_stroke(r, 0.0, Stroke::new(3.0, Color32::from_black_alpha(140)));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE));
        }
    }

    /// The Wand bar in Object Selection mode.
    pub(crate) fn object_select_bar(&mut self, ui: &mut egui::Ui, tight: bool) {
        crate::options_bar::select_ops(ui, &mut self.select_op);
        ui.separator();
        check(ui, &mut self.sample_merged, "All layers")
            .on_hover_text("Find objects in the merged image instead of the active layer only");
        ui.separator();
        let running = self
            .ai
            .running
            .as_ref()
            .filter(|r| matches!(r.task, AiTask::Object { .. }) && !r.job.cancelled())
            .map(|r| r.task.doing_short(r.job.progress.phase()));
        if let Some(why) = self.ai.service.unavailable() {
            ui.label(RichText::new("AI not available").color(MUTED))
                .on_hover_text(why);
        } else if let Some(doing) = running {
            ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
            ui.label(RichText::new(doing).color(TEXT));
        } else if !self.ai.installed(ModelKey::MobileSam) {
            let info = self.ai.info(ModelKey::MobileSam);
            let label = if tight {
                "Get model…".to_string()
            } else {
                format!("Get the model ({})…", format_bytes(info.bytes))
            };
            if ui
                .add(egui::Button::new(label))
                .on_hover_text(format!(
                    "Object Selection needs {} ({}, {} licence); it runs on this computer",
                    info.name,
                    format_bytes(info.bytes),
                    info.licence
                ))
                .clicked()
                && self.ai.consent.is_none()
            {
                self.ai.consent = Some(Consent {
                    model: ModelKey::MobileSam,
                    pending: None,
                    phase: ConsentPhase::Ask,
                });
            }
        } else if !tight {
            ui.label(
                RichText::new("Click an object or drag a box around it · Shift adds, Alt subtracts").weak(),
            );
        }
    }

    // ---- actions ---------------------------------------------------------------------

    /// Why an AI action can't run (`Some(None)` when it can), or `None`
    /// for ids that aren't AI actions.
    pub(crate) fn ai_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        if !matches!(id, SELECT_SUBJECT | REMOVE_BG | OBJECT_TOOL | AI_MODELS) {
            return None;
        }
        if id == AI_MODELS {
            return Some(None);
        }
        if self.ai.service.unavailable().is_some() {
            return Some(Some("AI features aren't available in this build"));
        }
        Some(match id {
            SELECT_SUBJECT if self.editor.doc().layer_count() == 0 => Some("The document is empty"),
            REMOVE_BG if !self.active_is_pixel() => Some("Select a pixel layer first"),
            _ => None,
        })
    }

    /// Run an AI action; false for ids that aren't AI actions.
    pub(crate) fn run_ai_action(&mut self, id: &str) -> bool {
        match id {
            SELECT_SUBJECT => self.ai_request(AiTask::Subject),
            REMOVE_BG => {
                if let Some(layer) = self.active {
                    self.ai_request(AiTask::RemoveBackground { layer });
                }
            }
            OBJECT_TOOL => {
                self.tool = Tool::Wand;
                self.set_wand_mode(crate::quick_select_tool::WandMode::Object);
            }
            AI_MODELS => {
                self.dialog = Some(Dialog::Preferences(self.prefs.clone(), None));
                self.ai.prefs_tab = true;
            }
            _ => return false,
        }
        true
    }

    // ---- debug -----------------------------------------------------------------------

    /// `--screenshot-do` tokens (`ai:...`): `ai:fake` (a fake engine with
    /// nothing installed and visible delays), `ai:fake-installed`,
    /// `ai:fake-offline`; `ai:consent=sam|birefnet` opens the first-use
    /// dialog, `ai:download=sam|birefnet` starts its download there and
    /// `ai:fetch=sam|birefnet` without it (as Preferences does);
    /// `ai:object` picks Object Selection; `ai:select=X:Y[:shift|alt]` and
    /// `ai:box=X0:Y0:X1:Y1` click or drag at document pixels;
    /// `ai:subject`, `ai:remove-bg`, `ai:prefs` run those actions;
    /// `ai:wait` blocks until every job is done and applied.
    pub(crate) fn debug_ai(&mut self, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("ai:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<f32> = arg.split(':').filter_map(|s| s.parse().ok()).collect();
        match verb {
            "fake" | "fake-installed" | "fake-offline" => {
                let installed: &[ModelKey] = if verb == "fake-installed" {
                    &ModelKey::ALL
                } else {
                    &[]
                };
                self.ai_use_service(Arc::new(FakeAi {
                    offline: std::sync::atomic::AtomicBool::new(verb == "fake-offline"),
                    steps: 40,
                    tick: Duration::from_millis(60),
                    encode_delay: Duration::from_millis(2500),
                    run_delay: Duration::from_millis(150),
                    ..FakeAi::new(installed)
                }));
            }
            "consent" | "download" => {
                let Some(model) = ModelKey::from_slug(arg) else {
                    return false;
                };
                self.ai.consent = Some(Consent {
                    model,
                    pending: None,
                    phase: ConsentPhase::Ask,
                });
                if verb == "download" {
                    self.ai_consent_download();
                }
            }
            "fetch" => {
                let Some(model) = ModelKey::from_slug(arg) else {
                    return false;
                };
                self.ai_start_download(model);
            }
            "object" => {
                self.tool = Tool::Wand;
                self.set_wand_mode(crate::quick_select_tool::WandMode::Object);
            }
            "select" if nums.len() >= 2 => {
                let op = match arg.rsplit(':').next() {
                    Some("shift") => CombineOp::Union,
                    Some("alt") => CombineOp::Subtract,
                    _ => self.select_op,
                };
                let prompt = AiPrompt::Point {
                    x: nums[0],
                    y: nums[1],
                    positive: true,
                };
                self.ai_request(AiTask::Object { prompt, op });
            }
            "box" if nums.len() == 4 => {
                let prompt = AiPrompt::Box {
                    x0: nums[0],
                    y0: nums[1],
                    x1: nums[2],
                    y1: nums[3],
                };
                let op = self.select_op;
                self.ai_request(AiTask::Object { prompt, op });
            }
            "subject" => self.run_menu_action(SELECT_SUBJECT),
            "remove-bg" => self.run_menu_action(REMOVE_BG),
            "prefs" => self.run_menu_action(AI_MODELS),
            "wait" => self.ai_wait(Duration::from_secs(60)),
            _ => return false,
        }
        true
    }
}

/// A thin determinate bar, its figures left to a label beside it (egui's
/// own text sits in the bar's ink and vanishes past the filled part).
fn progress_bar(ui: &mut egui::Ui, frac: f32, width: f32, name: &str) {
    ui.scope(|ui| {
        // A track that shows on every surface it sits on.
        ui.visuals_mut().extreme_bg_color = CONTROL;
        let bar = ui.add(
            egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                .desired_width(width)
                .desired_height(8.0),
        );
        a11y_name(&bar, &format!("{name} {:.0}%", frac * 100.0));
    });
}

/// Name, size, source and licence, label-left.
fn model_facts(ui: &mut egui::Ui, info: &ModelInfoView) {
    let fact = |ui: &mut egui::Ui, label: &str, value: String| {
        ui.horizontal(|ui| {
            row_label(ui, label, LABEL_W);
            ui.add(egui::Label::new(RichText::new(value).color(TEXT)).wrap());
        });
    };
    ui.spacing_mut().item_spacing.y = 4.0;
    fact(ui, "Model", info.name.clone());
    fact(ui, "Used by", info.purpose.clone());
    fact(ui, "Size", format_bytes(info.bytes));
    fact(ui, "Source", info.source.clone());
    fact(ui, "Licence", info.licence.clone());
    ui.spacing_mut().item_spacing.y = 8.0;
}

#[cfg(test)]
#[path = "ai_tests.rs"]
mod tests;
