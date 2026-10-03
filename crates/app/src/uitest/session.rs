//! A user session: the real app driven through real input events, one
//! frame at a time, with everything recorded. See docs/testing/harness.md.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, accesskit::Role, Event, Modifiers, PointerButton, Pos2, Vec2};
use lumenply_doc::Document;

use crate::sys_dialog::DialogAbout;
use crate::App;

use super::a11y::{Node, Tree};
use super::gpu::Gpu;
use super::input;
use super::record::{self, Overlay, Painter, Video};
use super::seam::SharedClip;
use super::ui_thread::{Reply, UiState, UiThread};

/// Why a session step could not go on.
#[derive(Clone, Debug)]
pub(crate) struct UiError(pub String);

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for UiError {
    fn from(s: String) -> Self {
        UiError(s)
    }
}

impl From<&str> for UiError {
    fn from(s: &str) -> Self {
        UiError(s.to_string())
    }
}

pub(crate) type UiResult<T = ()> = Result<T, UiError>;

/// What a failed check does to the scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnFail {
    /// Log it (with a screenshot) and carry on.
    Continue,
    /// Log it and end the scenario there.
    Stop,
}

#[derive(Clone, Debug)]
pub(crate) struct Options {
    /// The session's folder: video, stills, log, scratch files, profile.
    pub out: PathBuf,
    /// Window size in points.
    pub size: Vec2,
    pub pixels_per_point: f32,
    /// Frames per second of virtual time, and of the video.
    pub fps: u32,
    /// Render frames and record the video (needs a GPU; ffmpeg for video).
    pub record: bool,
    /// Record every Nth frame (1: all).
    pub record_every: u32,
    /// How long each step's last frame stays on screen in the video.
    pub hold_secs: f32,
    /// Save each step's last frame as a PNG.
    pub keyframes: bool,
    /// Scale the video (0.5 halves a 2× session).
    pub video_scale: f32,
    pub on_fail: OnFail,
    /// Command line for the app (empty: the welcome screen, as a user
    /// starting it from the Dock sees it).
    pub args: Vec<String>,
    /// Show the startup splash (off: sessions start on the UI at once).
    pub splash: bool,
    /// The longest one frame may take before the app counts as hung.
    pub frame_timeout: Duration,
}

impl Options {
    pub(crate) fn new(out: impl Into<PathBuf>) -> Options {
        Options {
            out: out.into(),
            size: egui::vec2(1440.0, 900.0),
            pixels_per_point: 1.0,
            fps: 30,
            record: true,
            record_every: 1,
            hold_secs: 0.6,
            keyframes: true,
            video_scale: 1.0,
            on_fail: OnFail::Continue,
            args: Vec::new(),
            splash: false,
            frame_timeout: Duration::from_secs(90),
        }
    }
}

/// One line of the session log.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct StepRecord {
    pub index: usize,
    /// "launch", "action", "system dialog", "check", "wait" or "note".
    pub kind: &'static str,
    /// What the caption said.
    pub description: String,
    /// The harness call, in words.
    pub action: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    /// "pass", "fail" or "info".
    pub result: &'static str,
    pub error: Option<String>,
    /// Wall-clock milliseconds since the session started.
    pub started_ms: u64,
    pub finished_ms: u64,
    /// Where the step is in the video, in seconds.
    pub video_from_s: f64,
    pub video_to_s: f64,
    /// App frames run during the step.
    pub frames: u64,
    pub screenshot: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SessionLog {
    pub scenario: String,
    pub result: &'static str,
    pub error: Option<String>,
    pub failed_checks: usize,
    pub window: [f32; 2],
    pub pixels_per_point: f32,
    pub fps: u32,
    pub frames: u64,
    pub video: Option<String>,
    pub video_seconds: f64,
    pub wall_seconds: f64,
    pub gpu: Option<String>,
    pub data_dir: String,
    pub notes: Vec<String>,
    pub steps: Vec<StepRecord>,
}

/// What a finished session left behind.
#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub name: String,
    pub passed: bool,
    pub steps: usize,
    pub failed: Vec<String>,
    pub video: Option<PathBuf>,
    pub log: PathBuf,
}

/// One frame's output, gathered on the UI thread.
pub(crate) struct FrameOut {
    textures: egui::TexturesDelta,
    prims: Vec<egui::ClippedPrimitive>,
    ppp: f32,
    a11y: Option<egui::accesskit::TreeUpdate>,
    texts: Vec<(String, egui::Rect)>,
    copied: String,
    repaint: Duration,
    commands: Vec<egui::ViewportCommand>,
    open_url: Option<String>,
}

/// Painted text, with where it shows (clipped), for "text shown" checks.
fn collect_texts(shapes: &[egui::epaint::ClippedShape], screen: egui::Rect) -> Vec<(String, egui::Rect)> {
    fn walk(shape: &egui::Shape, clip: egui::Rect, out: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(t) => {
                let r = t.visual_bounding_rect().intersect(clip);
                if r.is_positive() && !t.galley.text().trim().is_empty() {
                    out.push((t.galley.text().to_string(), r));
                }
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        walk(&s.shape, s.clip_rect.intersect(screen), &mut out);
    }
    out
}

/// One frame, as eframe runs it: the raw-input hook, then `update`.
fn run_frame(st: &mut UiState, mut raw: egui::RawInput) -> FrameOut {
    let UiState { app, ctx } = st;
    let screen = raw.screen_rect.unwrap_or(egui::Rect::EVERYTHING);
    eframe::App::raw_input_hook(app, ctx, &mut raw);
    let out = ctx.run(raw, |ctx| app.frame(ctx));
    let texts = collect_texts(&out.shapes, screen);
    let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
    let vp = out.viewport_output.get(&egui::ViewportId::ROOT);
    FrameOut {
        textures: out.textures_delta,
        prims,
        ppp: out.pixels_per_point,
        a11y: out.platform_output.accesskit_update,
        texts,
        copied: out.platform_output.copied_text,
        repaint: vp.map_or(Duration::MAX, |v| v.repaint_delay),
        commands: vp.map(|v| v.commands.clone()).unwrap_or_default(),
        open_url: out.platform_output.open_url.map(|u| u.url),
    }
}

impl App {
    /// Work still going on behind the UI, by name (empty: none).
    pub(crate) fn uitest_busy(&self) -> Vec<&'static str> {
        let mut busy = Vec::new();
        if self.ai.active() {
            busy.push("AI job");
        }
        if self.export_as.as_ref().is_some_and(|e| e.encoding()) {
            busy.push("export preview");
        }
        // (The welcome screen leaves `dirty` set: there is nothing to draw.)
        if self.dirty && !self.no_doc {
            busy.push("canvas refresh");
        }
        if self.splash_until.is_some_and(|t| t > Instant::now()) {
            busy.push("splash");
        }
        busy
    }
}

pub(crate) struct Session {
    pub(crate) name: String,
    opts: Options,
    dir: PathBuf,
    ui: UiThread,
    gpu: Option<Gpu>,
    painter: Painter,
    video: Option<Video>,
    /// The last frame as rendered (no overlay).
    ui_image: Option<image::RgbaImage>,

    // Input state, as the window system would keep it.
    time: f64,
    pointer: Pos2,
    pointer_known: bool,
    pressed: Option<PointerButton>,
    modifiers: Modifiers,
    events: Vec<Event>,

    // The last frame, as the user saw it, and the one before.
    tree: Tree,
    prev_tree: Tree,
    texts: Vec<(String, egui::Rect)>,
    repaint: Duration,
    frames: u64,
    recorded: u64,
    pub(crate) title: String,
    quit: bool,

    // Stand-ins for the system.
    clipboard_text: String,
    clipboard_image: SharedClip,
    dialog: Option<DialogAbout>,
    crashed: Option<String>,

    /// `within(area, ...)`: lookups only see controls inside this one.
    scope: Option<String>,

    // The log.
    steps: Vec<StepRecord>,
    caption: String,
    step_no: usize,
    next_caption: Option<String>,
    started: Instant,
    notes: Vec<String>,
    failed_checks: usize,
    data_dir: PathBuf,
}

fn frames_for(dist: f32) -> usize {
    // About 1.2 points per millisecond, like a quick hand, never instant.
    ((dist / 40.0).ceil() as usize).clamp(4, 16)
}

fn ease(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

impl Session {
    // ---- life cycle ----------------------------------------------------

    /// Launch Lumenply as a user would (by default on its welcome screen)
    /// in a fresh scratch profile, and start recording.
    pub(crate) fn start(name: &str, opts: Options) -> UiResult<Session> {
        let dir = std::path::absolute(&opts.out).map_err(|e| e.to_string())?;
        // Replace an earlier run of this session, never anything else.
        if dir.exists() {
            let earlier = dir.join("session.json").exists()
                || std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none());
            if !earlier {
                return Err(UiError(format!(
                    "{} exists and is not an earlier session; choose another --out",
                    dir.display()
                )));
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::create_dir_all(dir.join("files")).map_err(|e| format!("{}: {e}", dir.display()))?;
        if !opts.splash {
            std::env::set_var("LUMENPLY_SPLASH_MS", "0");
        }
        // Outside `cargo test` the profile is a folder of the session's own,
        // and HOME follows it, so nothing of the user's is read or written.
        // Under `cargo test` the app already keeps a per-thread test folder.
        let profile = dir.join("profile");
        if !cfg!(test) {
            std::fs::create_dir_all(profile.join("home")).map_err(|e| e.to_string())?;
            std::env::set_var("LUMENPLY_DATA_DIR", &profile);
            std::env::set_var("HOME", profile.join("home"));
        }
        let expect = (!cfg!(test)).then(|| profile.clone());
        let args = opts.args.clone();
        let clipboard: SharedClip = Default::default();
        let (data_tx, data_rx) = std::sync::mpsc::channel();
        let make = move || {
            let data = crate::session::data_dir().expect("a data folder");
            match &expect {
                Some(p) => assert_eq!(&data, p, "the session must run in its scratch profile"),
                None => assert!(
                    data.starts_with(std::env::temp_dir()),
                    "a test profile must be temporary: {}",
                    data.display()
                ),
            }
            let _ = std::fs::remove_dir_all(&data);
            let _ = std::fs::create_dir_all(&data);
            let _ = data_tx.send(data);
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            ctx.enable_accesskit();
            let app = App::launch(&args);
            UiState { app, ctx }
        };
        let ui = UiThread::start(format!("uitest-{name}"), clipboard.clone(), make)?;
        let data_dir = data_rx.recv().unwrap_or_default();

        let ppp = opts.pixels_per_point;
        let px = [
            (opts.size.x * ppp).round() as u32,
            (opts.size.y * ppp).round() as u32,
        ];
        let mut notes = Vec::new();
        let gpu = if opts.record {
            match Gpu::new(px) {
                Ok(g) => Some(g),
                Err(e) => {
                    notes.push(e);
                    None
                }
            }
        } else {
            None
        };
        let mut painter = Painter::new(ppp);
        let video = match &gpu {
            Some(_) => {
                let probe = painter.compose(
                    &image::RgbaImage::new(px[0], px[1]),
                    &Overlay {
                        pointer: None,
                        pressed: None,
                        step: 0,
                        caption: "",
                        outcome: None,
                        dialog: None,
                    },
                );
                match Video::start(
                    &dir.join("session.mp4"),
                    [probe.width(), probe.height()],
                    opts.fps,
                    opts.video_scale,
                ) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        notes.push(e);
                        None
                    }
                }
            }
            None => None,
        };
        let mut s = Session {
            name: name.to_string(),
            dir,
            ui,
            gpu,
            painter,
            video,
            ui_image: None,
            time: 0.0,
            pointer: egui::pos2(opts.size.x * 0.5, opts.size.y * 0.55),
            pointer_known: false,
            pressed: None,
            modifiers: Modifiers::NONE,
            events: Vec::new(),
            tree: Tree::default(),
            prev_tree: Tree::default(),
            texts: Vec::new(),
            repaint: Duration::ZERO,
            frames: 0,
            recorded: 0,
            title: String::new(),
            quit: false,
            clipboard_text: String::new(),
            clipboard_image: clipboard,
            dialog: None,
            crashed: None,
            scope: None,
            steps: Vec::new(),
            caption: String::new(),
            step_no: 0,
            next_caption: None,
            started: Instant::now(),
            notes,
            failed_checks: 0,
            data_dir,
            opts,
        };
        if let Err(e) = s.step("launch", "Launch Lumenply".into(), |s| {
            s.settle_idle(Duration::from_secs(60))
        }) {
            // Still leave a log and whatever video there is.
            s.finish(Err(e.clone()));
            return Err(e);
        }
        Ok(s)
    }

    /// End the session: close the video and write `session.json`.
    pub(crate) fn finish(mut self, outcome: UiResult) -> Summary {
        let error = outcome.err().map(|e| e.0);
        if let Some(e) = &error {
            // The failing step already has its screenshot; a scenario
            // error outside any step gets one here.
            if self.steps.last().is_none_or(|s| s.result != "fail") {
                let shot = self.save_shot("failures", "scenario-error");
                self.notes
                    .push(format!("scenario stopped: {e} ({})", shot.unwrap_or_default()));
            }
        }
        let video_seconds = self.video_time();
        let video = self.video.take().map(|v| v.finish());
        let video = match video {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                self.notes.push(e);
                None
            }
            None => None,
        };
        let failed: Vec<String> = self
            .steps
            .iter()
            .filter(|s| s.result == "fail")
            .map(|s| format!("{}: {}", s.description, s.error.clone().unwrap_or_default()))
            .collect();
        let passed = error.is_none() && failed.is_empty();
        let log = SessionLog {
            scenario: self.name.clone(),
            result: if passed { "pass" } else { "fail" },
            error,
            failed_checks: self.failed_checks,
            window: [self.opts.size.x, self.opts.size.y],
            pixels_per_point: self.opts.pixels_per_point,
            fps: self.opts.fps,
            frames: self.frames,
            video: video
                .as_ref()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned()),
            video_seconds,
            wall_seconds: self.started.elapsed().as_secs_f64(),
            gpu: self.gpu.as_ref().map(|g| g.adapter.clone()),
            data_dir: self.data_dir.display().to_string(),
            notes: self.notes.clone(),
            steps: self.steps.clone(),
        };
        let log_path = self.dir.join("session.json");
        if let Ok(json) = serde_json::to_string_pretty(&log) {
            let _ = std::fs::write(&log_path, json);
        }
        Summary {
            name: self.name.clone(),
            passed,
            steps: self.steps.len(),
            failed,
            video,
            log: log_path,
        }
    }

    /// The session's folder for the user's files (copies, saves).
    pub(crate) fn files(&self) -> PathBuf {
        self.dir.join("files")
    }

    /// Copy a file into the session's folder, as the user's own copy.
    pub(crate) fn copy_in(&self, src: impl AsRef<Path>) -> UiResult<PathBuf> {
        let src = src.as_ref();
        let dst = self.files().join(src.file_name().ok_or("no file name")?);
        std::fs::copy(src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        Ok(dst)
    }

    // ---- frames ----------------------------------------------------------

    fn guard(&self) -> UiResult {
        if let Some(c) = &self.crashed {
            return Err(UiError(format!("Lumenply crashed earlier: {c}")));
        }
        if let Some(d) = &self.dialog {
            return Err(UiError(format!(
                "Lumenply is waiting in the system {} panel “{}”: answer it with \
                 choose_file / save_as / cancel_dialog first",
                d.kind, d.title
            )));
        }
        if self.quit {
            return Err("Lumenply has quit".into());
        }
        Ok(())
    }

    fn raw_input(&mut self) -> egui::RawInput {
        let screen = egui::Rect::from_min_size(Pos2::ZERO, self.opts.size);
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            max_texture_side: Some(self.gpu.as_ref().map_or(8192, |g| g.max_texture_side)),
            time: Some(self.time),
            predicted_dt: 1.0 / self.opts.fps as f32,
            modifiers: self.modifiers,
            events: std::mem::take(&mut self.events),
            focused: true,
            system_theme: Some(egui::Theme::Dark),
            ..Default::default()
        };
        let info = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
        info.native_pixels_per_point = Some(self.opts.pixels_per_point);
        info.inner_rect = Some(screen);
        info.outer_rect = Some(screen);
        info.focused = Some(true);
        raw
    }

    /// Run one frame with the queued input events.
    fn frame(&mut self) -> UiResult {
        self.guard()?;
        let raw = self.raw_input();
        self.ui.send(move |st: &mut UiState| run_frame(st, raw));
        self.receive_frame()
    }

    fn receive_frame(&mut self) -> UiResult {
        match self.ui.recv(self.opts.frame_timeout).map_err(UiError)? {
            Reply::Done(Ok(any)) => match any.downcast::<FrameOut>() {
                Ok(out) => {
                    self.absorb(*out);
                    Ok(())
                }
                Err(_) => Err("internal: a frame returned something else".into()),
            },
            Reply::Done(Err(p)) => {
                self.crashed = Some(p.clone());
                Err(UiError(format!("Lumenply panicked: {p}")))
            }
            Reply::Dialog(about) => {
                self.dialog = Some(about);
                Ok(())
            }
        }
    }

    fn absorb(&mut self, out: FrameOut) {
        self.time += 1.0 / self.opts.fps as f64;
        self.frames += 1;
        if let Some(u) = &out.a11y {
            self.prev_tree = std::mem::replace(&mut self.tree, Tree::from_update(u));
        }
        self.texts = out.texts;
        self.repaint = out.repaint;
        if !out.copied.is_empty() {
            self.clipboard_text = out.copied;
        }
        if let Some(url) = out.open_url {
            self.notes.push(format!("the app asked to open {url}"));
        }
        for c in out.commands {
            match c {
                egui::ViewportCommand::Close => self.quit = true,
                egui::ViewportCommand::Title(t) => self.title = t,
                _ => {}
            }
        }
        if let Some(gpu) = self.gpu.as_mut() {
            self.ui_image = Some(gpu.paint(&out.textures, &out.prims, out.ppp));
            self.recorded += 1;
            if self.recorded % self.opts.record_every.max(1) as u64 == 0 {
                self.record(None, true);
            }
        }
    }

    /// The frame as the video shows it now.
    fn composed(&mut self, outcome: Option<bool>) -> Option<image::RgbaImage> {
        let ui = self.ui_image.as_ref()?;
        let lines = self.dialog.as_ref().map(dialog_lines);
        let o = Overlay {
            pointer: Some(self.pointer),
            pressed: self.pressed,
            step: self.step_no,
            caption: &self.caption,
            outcome,
            dialog: lines.as_deref(),
        };
        Some(self.painter.compose(ui, &o))
    }

    fn record(&mut self, outcome: Option<bool>, dedupe: bool) {
        if self.video.is_none() {
            return;
        }
        if let Some(f) = self.composed(outcome) {
            if let Some(v) = self.video.as_mut() {
                v.push(&f, dedupe);
            }
        }
    }

    fn video_time(&self) -> f64 {
        self.video
            .as_ref()
            .map_or(0.0, |v| v.frames as f64 / self.opts.fps as f64)
    }

    /// Frames until the UI stops asking to be redrawn at once (no
    /// animation, no follow-up frame), at most `max`.
    fn settle(&mut self, max: usize) -> UiResult {
        for _ in 0..max {
            // A file panel the action opened waits for the scenario.
            if self.dialog.is_some() {
                return Ok(());
            }
            self.frame()?;
            if self.repaint >= Duration::from_millis(40) {
                break;
            }
        }
        Ok(())
    }

    /// Frames until the UI is still and no background work is left.
    fn settle_idle(&mut self, timeout: Duration) -> UiResult {
        let start = Instant::now();
        let mut still = 0;
        loop {
            if self.dialog.is_some() {
                return Ok(());
            }
            self.frame()?;
            if self.dialog.is_some() {
                return Ok(());
            }
            let busy = self.call(|st| st.app.uitest_busy())?;
            if busy.is_empty() && self.repaint >= Duration::from_millis(40) {
                still += 1;
                if still >= 2 {
                    return Ok(());
                }
            } else {
                still = 0;
            }
            if start.elapsed() > timeout {
                return Err(UiError(format!(
                    "still busy after {:.0} s: {}",
                    timeout.as_secs_f32(),
                    if busy.is_empty() {
                        "the UI keeps animating".to_string()
                    } else {
                        busy.join(", ")
                    }
                )));
            }
            if !busy.is_empty() {
                // Give background threads real time; frames are virtual.
                std::thread::sleep(Duration::from_millis(8));
            }
        }
    }

    /// Run `f` on the UI thread with the app (no input, no frame).
    fn call<R: Send + 'static>(&mut self, f: impl FnOnce(&mut UiState) -> R + Send + 'static) -> UiResult<R> {
        if let Some(c) = &self.crashed {
            return Err(UiError(format!("Lumenply crashed earlier: {c}")));
        }
        if self.dialog.is_some() {
            return Err("a system file panel is open".into());
        }
        self.ui.send(f);
        loop {
            match self.ui.recv(self.opts.frame_timeout).map_err(UiError)? {
                Reply::Done(Ok(any)) => {
                    return any
                        .downcast::<R>()
                        .map(|b| *b)
                        .map_err(|_| "internal: unexpected reply".into())
                }
                Reply::Done(Err(p)) => {
                    self.crashed = Some(p.clone());
                    return Err(UiError(format!("Lumenply panicked: {p}")));
                }
                Reply::Dialog(d) => {
                    self.notes
                        .push(format!("a check opened the panel “{}”; cancelled it", d.title));
                    self.ui.answer(None);
                }
            }
        }
    }

    // ---- the step log ----------------------------------------------------

    /// Use `text` as the caption (and log description) of the next step.
    pub(crate) fn describe(&mut self, text: &str) -> &mut Self {
        self.next_caption = Some(text.to_string());
        self
    }

    /// Run one logged step. Actions that fail end the scenario (the error
    /// comes back); the step gets a screenshot either way when it fails.
    fn step<T>(
        &mut self,
        kind: &'static str,
        action: String,
        body: impl FnOnce(&mut Session) -> UiResult<T>,
    ) -> UiResult<T> {
        let idx = self.begin(kind, action);
        let r = body(self);
        let outcome = r.as_ref().map(|_| ()).map_err(|e| e.0.clone());
        self.end(idx, outcome, None, None, kind != "wait");
        r
    }

    fn begin(&mut self, kind: &'static str, action: String) -> usize {
        let description = self.next_caption.take().unwrap_or_else(|| action.clone());
        self.step_no += 1;
        self.caption = description.clone();
        let now = self.started.elapsed().as_millis() as u64;
        self.steps.push(StepRecord {
            index: self.step_no,
            kind,
            description,
            action,
            expected: None,
            actual: None,
            result: "info",
            error: None,
            started_ms: now,
            finished_ms: now,
            video_from_s: self.video_time(),
            video_to_s: 0.0,
            frames: self.frames,
            screenshot: None,
        });
        self.steps.len() - 1
    }

    fn end(
        &mut self,
        idx: usize,
        outcome: Result<(), String>,
        expected: Option<String>,
        actual: Option<String>,
        hold: bool,
    ) {
        let ok = outcome.is_ok();
        let kind = self.steps[idx].kind;
        let mut shot = None;
        if !ok {
            let name = format!(
                "{:03}-{}",
                self.steps[idx].index,
                record::slug(&self.steps[idx].description)
            );
            shot = self.save_shot("failures", &name);
        } else if self.opts.keyframes && kind != "wait" {
            let name = format!(
                "{:03}-{}",
                self.steps[idx].index,
                record::slug(&self.steps[idx].description)
            );
            shot = self.save_shot("keyframes", &name);
        }
        let mark = if kind == "note" || kind == "wait" {
            None
        } else {
            Some(ok)
        };
        if hold {
            let n = (self.opts.hold_secs * self.opts.fps as f32).round() as u32;
            let n = if kind == "check" || kind == "note" {
                n.div_ceil(2).max(n.min(12))
            } else {
                n
            };
            if let Some(f) = self.composed(mark) {
                if let Some(v) = self.video.as_mut() {
                    v.hold(&f, n);
                }
            }
        }
        let video_to = self.video_time();
        let frames = self.frames - self.steps[idx].frames;
        let st = &mut self.steps[idx];
        st.finished_ms = self.started.elapsed().as_millis() as u64;
        st.video_to_s = video_to;
        st.frames = frames;
        st.expected = expected;
        st.actual = actual;
        st.screenshot = shot;
        match outcome {
            Ok(()) => st.result = if mark.is_some() { "pass" } else { "info" },
            Err(e) => {
                st.result = "fail";
                st.error = Some(e);
            }
        }
    }

    /// Save the current frame (as the video shows it) under `folder/`.
    fn save_shot(&mut self, folder: &str, name: &str) -> Option<String> {
        let f = self.composed(None)?;
        let rel = format!("{folder}/{name}.png");
        record::save_png(&f, &self.dir.join(&rel)).ok()?;
        Some(rel)
    }

    /// Save a still of the current frame as `stills/<name>.png`.
    pub(crate) fn screenshot(&mut self, name: &str) -> Option<PathBuf> {
        let rel = self.save_shot("stills", &record::slug(name))?;
        Some(self.dir.join(rel))
    }

    /// Write every named control on screen to `trees/<label>.txt` (for
    /// finding the names a scenario should use).
    pub(crate) fn dump_tree(&mut self, label: &str) -> PathBuf {
        let path = self
            .dir
            .join("trees")
            .join(format!("{:03}-{}.txt", self.step_no, record::slug(label)));
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(&path, self.tree.dump());
        path
    }

    /// The last rendered frame of the UI itself (no pointer or caption).
    pub(crate) fn ui_image(&self) -> Option<&image::RgbaImage> {
        self.ui_image.as_ref()
    }

    /// A line in the log and caption, with no action.
    pub(crate) fn note(&mut self, text: &str) -> UiResult {
        let idx = self.begin("note", text.to_string());
        self.end(idx, Ok(()), None, None, true);
        Ok(())
    }

    // ---- finding controls ------------------------------------------------

    /// The accessibility tree of the last frame.
    pub(crate) fn tree(&self) -> &Tree {
        &self.tree
    }

    fn screen(&self) -> egui::Rect {
        egui::Rect::from_min_size(Pos2::ZERO, self.opts.size)
    }

    fn visible(&self, n: &Node) -> bool {
        let r = n.rect.intersect(self.screen());
        r.width() >= 1.0 && r.height() >= 1.0
    }

    /// The node a person would mean by `name`, once it has settled: it
    /// must look the same in two frames running (a menu's first frame is
    /// an invisible, inert sizing pass). Waits up to `wait` frames for it
    /// to appear.
    fn find(&mut self, role: Option<Role>, name: &str, wait: usize) -> UiResult<Node> {
        let screen = self.screen();
        let scope = self.scope.clone();
        let area = |tree: &Tree| match &scope {
            Some(a) => pick(tree, None, a, screen, None).ok().flatten().map(|n| n.rect),
            None => Some(screen),
        };
        let mut last = None;
        for attempt in 0..=wait + 4 {
            let Some(within) = area(&self.tree) else {
                if attempt >= wait {
                    break;
                }
                self.frame()?;
                continue;
            };
            let now = pick(&self.tree, role, name, screen, Some(within)).map_err(UiError)?;
            if let Some(n) = &now {
                let before = area(&self.prev_tree)
                    .and_then(|w| pick(&self.prev_tree, role, name, screen, Some(w)).ok().flatten());
                if before.is_some_and(|b| b.rect == n.rect && b.disabled == n.disabled) {
                    return Ok(n.clone());
                }
            } else if attempt >= wait {
                break;
            }
            last = now.or(last);
            self.frame()?;
        }
        if let Some(n) = last {
            return Ok(n); // it keeps moving; act on where it is
        }
        let hidden: Vec<String> = self
            .tree
            .matches(name, role)
            .iter()
            .map(|n| n.describe())
            .collect();
        if !hidden.is_empty() {
            return Err(UiError(format!(
                "“{name}” is not on screen: {}",
                hidden.join("; ")
            )));
        }
        let close = self.tree.close_matches(name, role);
        let what = match (role, &self.scope) {
            (Some(r), None) => format!("{r:?} “{name}”"),
            (None, None) => format!("“{name}”"),
            (Some(r), Some(a)) => format!("{r:?} “{name}” in “{a}”"),
            (None, Some(a)) => format!("“{name}” in “{a}”"),
        };
        Err(UiError(if close.is_empty() {
            format!("no control named {what} on screen")
        } else {
            format!(
                "no control named {what} on screen; close matches: {}",
                close.join(", ")
            )
        }))
    }

    /// Run `f` with lookups limited to controls inside the control or
    /// panel named `area`: `within("Properties", |s| s.drag_slider("Opacity", 0.5))`
    /// when the options bar has an "Opacity" too.
    pub(crate) fn within<T>(
        &mut self,
        area: &str,
        f: impl FnOnce(&mut Session) -> UiResult<T>,
    ) -> UiResult<T> {
        self.find(None, area, 12)?;
        let before = self.scope.replace(area.to_string());
        let r = f(self);
        self.scope = before;
        r
    }

    /// Whether a control named `name` is on screen now.
    pub(crate) fn has_node(&self, name: &str) -> bool {
        self.tree.matches(name, None).iter().any(|n| self.visible(n))
    }

    /// The control named `name` (no waiting), for reading its state.
    pub(crate) fn node(&self, name: &str) -> Option<Node> {
        self.tree
            .matches(name, None)
            .into_iter()
            .filter(|n| self.visible(n))
            .last()
            .cloned()
    }

    fn target(&self, n: &Node) -> Pos2 {
        n.rect.intersect(self.screen()).center()
    }

    // ---- low-level input ---------------------------------------------------

    /// Glide the pointer to `to` over a few frames, as a hand would.
    fn glide(&mut self, to: Pos2) -> UiResult {
        let from = self.pointer;
        if self.pointer_known && (to - from).length() < 0.5 {
            return Ok(());
        }
        let n = if self.pointer_known {
            frames_for((to - from).length())
        } else {
            6
        };
        for i in 1..=n {
            let p = from + (to - from) * ease(i as f32 / n as f32);
            self.pointer = p;
            self.events.push(Event::PointerMoved(p));
            self.frame()?;
        }
        self.pointer_known = true;
        Ok(())
    }

    fn button(&mut self, button: PointerButton, pressed: bool) -> UiResult {
        self.pressed = pressed.then_some(button);
        self.events.push(Event::PointerButton {
            pos: self.pointer,
            button,
            pressed,
            modifiers: self.modifiers,
        });
        self.frame()
    }

    fn click_here(&mut self, button: PointerButton, count: usize) -> UiResult {
        for i in 0..count {
            self.button(button, true)?;
            self.frame()?;
            self.button(button, false)?;
            if i + 1 < count {
                self.frame()?;
            }
        }
        Ok(())
    }

    fn with_modifiers<T>(
        &mut self,
        mods: Modifiers,
        f: impl FnOnce(&mut Session) -> UiResult<T>,
    ) -> UiResult<T> {
        let before = self.modifiers;
        self.modifiers = mods;
        let r = f(self);
        self.modifiers = before;
        r
    }

    fn press_key(&mut self, mods: Modifiers, key: egui::Key) -> UiResult {
        self.modifiers = mods;
        let is = |k: egui::Key| key == k && mods.command;
        if is(egui::Key::C) {
            self.events.push(Event::Copy);
        } else if is(egui::Key::X) {
            self.events.push(Event::Cut);
        } else if is(egui::Key::V) {
            // As egui-winit: a Paste event only when the clipboard holds
            // text; on macOS the app's AppKit monitor sees every Cmd+V.
            if !self.clipboard_text.is_empty() {
                self.events.push(Event::Paste(self.clipboard_text.clone()));
            }
            if cfg!(target_os = "macos") {
                crate::clipboard::os_paste_key(mods.shift);
            }
        } else {
            self.events.push(Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: mods,
            });
            if let Some(t) = input::typed_text(key, mods) {
                self.events.push(Event::Text(t));
            }
        }
        self.frame()?;
        self.events.push(Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers: mods,
        });
        self.frame()?;
        self.modifiers = Modifiers::NONE;
        Ok(())
    }

    fn type_chars(&mut self, text: &str) -> UiResult {
        for ch in text.chars() {
            let key = input::key_for_char(ch);
            let mods = if ch.is_ascii_uppercase() {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            };
            self.modifiers = mods;
            if let Some(k) = key {
                self.events.push(Event::Key {
                    key: k,
                    physical_key: Some(k),
                    pressed: true,
                    repeat: false,
                    modifiers: mods,
                });
            }
            if ch != '\n' && ch != '\t' {
                self.events.push(Event::Text(ch.to_string()));
            }
            self.frame()?;
            if let Some(k) = key {
                self.events.push(Event::Key {
                    key: k,
                    physical_key: Some(k),
                    pressed: false,
                    repeat: false,
                    modifiers: mods,
                });
            }
        }
        self.modifiers = Modifiers::NONE;
        self.frame()
    }

    /// The labels a hover at `p` brings up (a tooltip), waiting for it.
    fn tooltip_at(&mut self, p: Pos2) -> UiResult<Vec<String>> {
        let before: std::collections::HashSet<String> = self.tree.labels().into_iter().collect();
        self.glide(p)?;
        // egui waits ~0.5 s of stillness before a tooltip.
        for _ in 0..(self.opts.fps as usize * 3 / 2) {
            self.frame()?;
            let new: Vec<String> = self
                .tree
                .labels()
                .into_iter()
                .filter(|l| !before.contains(l))
                .collect();
            if !new.is_empty() {
                self.frame()?;
                return Ok(self
                    .tree
                    .labels()
                    .into_iter()
                    .filter(|l| !before.contains(l))
                    .collect());
            }
        }
        Ok(Vec::new())
    }

    // ---- user actions --------------------------------------------------------

    /// The widgets under the pointer as egui hit-tests them: (hovered,
    /// containing the pointer), by accesskit id.
    fn under_pointer(&mut self) -> UiResult<(Vec<u64>, Vec<u64>)> {
        self.call(|st| {
            st.ctx.interaction_snapshot(|i| {
                (
                    i.hovered.iter().map(|id| id.value()).collect(),
                    i.contains_pointer.iter().map(|id| id.value()).collect(),
                )
            })
        })
    }

    /// Whether the pointer is on `n` itself, not on something covering it
    /// or on the edge of the scroll area that clips it.
    fn on_node(&mut self, n: &Node) -> UiResult<Result<(), String>> {
        let (hovered, contains) = self.under_pointer()?;
        if hovered.contains(&n.id) || (hovered.is_empty() && contains.contains(&n.id)) {
            return Ok(Ok(()));
        }
        let over: Vec<String> = hovered
            .iter()
            .filter_map(|id| self.tree.by_id(*id))
            .map(|o| format!("{:?} “{}”", o.role, o.name))
            .collect();
        Ok(Err(if over.is_empty() {
            "it is scrolled out of view or covered".to_string()
        } else {
            format!("{} is in the way", over.join(", "))
        }))
    }

    /// The scroll area a node hidden in it belongs to: one whose columns
    /// hold the node and that does not show all of it.
    fn scroll_view_for(&self, n: &Node) -> Option<Node> {
        let c = n.rect.center();
        self.tree
            .nodes
            .iter()
            .filter(|v| v.role == Role::ScrollView)
            .filter(|v| v.rect.x_range().contains(c.x) && !v.rect.contains_rect(n.rect))
            .min_by(|a, b| {
                let d = |v: &Node| (c.y - v.rect.center().y).abs();
                d(a).total_cmp(&d(b))
            })
            .cloned()
    }

    /// Find a control and put the pointer on it at the first of `points`
    /// (all of which the action needs to see), the way a person would:
    /// scrolling its panel when it is cut off or out of view, and refusing
    /// when something else covers it.
    fn aim(&mut self, role: Option<Role>, name: &str, points: &dyn Fn(&Node) -> Vec<Pos2>) -> UiResult<Node> {
        let mut scrolls = 0;
        loop {
            let n = self.find(role, name, 12)?;
            let pts = points(&n);
            let p = *pts.first().ok_or("no point to aim at")?;
            // Cut off by a scroll area it sits in: bring the points into view.
            let clip = self
                .tree
                .nodes
                .iter()
                .filter(|v| v.role == Role::ScrollView)
                .find(|v| v.rect.intersects(n.rect) && !v.rect.contains_rect(n.rect))
                .cloned();
            if let Some(view) = clip.filter(|_| scrolls < 4) {
                let inner = view.rect.shrink(4.0);
                if pts.iter().any(|q| !inner.contains(*q)) {
                    scrolls += 1;
                    let (lo, hi) = pts
                        .iter()
                        .fold((f32::MAX, f32::MIN), |(lo, hi), q| (lo.min(q.y), hi.max(q.y)));
                    self.glide(view.rect.center())?;
                    self.wheel((lo + hi) / 2.0 - view.rect.center().y)?;
                    continue;
                }
            }
            self.glide(p)?;
            let why = match self.on_node(&n)? {
                Ok(()) => return Ok(n),
                Err(why) => why,
            };
            // Hidden under the end of a list: scroll it into view.
            match self.scroll_view_for(&n).filter(|_| scrolls < 4) {
                Some(view) => {
                    scrolls += 1;
                    let dy = n.rect.center().y - view.rect.center().y;
                    self.glide(view.rect.center())?;
                    self.wheel(dy)?;
                }
                None => {
                    return Err(UiError(format!(
                        "can't reach “{name}” at ({:.0}, {:.0}): {why}",
                        p.x, p.y
                    )))
                }
            }
        }
    }

    /// Scroll the wheel by `dy` points (positive: down the page).
    fn wheel(&mut self, dy: f32) -> UiResult {
        for _ in 0..4 {
            self.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -dy / 4.0),
                modifiers: self.modifiers,
            });
            self.frame()?;
        }
        self.settle(40)
    }

    /// The reason a greyed-out control gives in its tooltip.
    fn greyed_out(&mut self, n: &Node) -> UiResult<UiError> {
        let why = self.tooltip_at(self.target(n))?;
        Ok(UiError(format!(
            "“{}” is greyed out{}",
            n.name,
            if why.is_empty() {
                String::new()
            } else {
                format!(": {}", why.join(" "))
            }
        )))
    }

    fn click_node(
        &mut self,
        role: Option<Role>,
        name: &str,
        button: PointerButton,
        count: usize,
        mods: Modifiers,
    ) -> UiResult {
        let n = self.find(role, name, 12)?;
        if n.disabled {
            return Err(self.greyed_out(&n)?);
        }
        let screen = self.screen();
        self.aim(role, name, &|n: &Node| vec![n.rect.intersect(screen).center()])?;
        self.with_modifiers(mods, |s| s.click_here(button, count))?;
        self.settle(20)
    }

    /// Click the control a user would call `name`.
    pub(crate) fn click(&mut self, name: &str) -> UiResult {
        self.step("action", format!("Click “{name}”"), |s| {
            s.click_node(None, name, PointerButton::Primary, 1, Modifiers::NONE)
        })
    }

    /// Click the `role` control named `name` (when a name is shared, e.g.
    /// a slider and its number field).
    pub(crate) fn click_role(&mut self, role: Role, name: &str) -> UiResult {
        self.step("action", format!("Click {role:?} “{name}”"), |s| {
            s.click_node(Some(role), name, PointerButton::Primary, 1, Modifiers::NONE)
        })
    }

    /// Click with keys held, e.g. `click_with("Layer 1", "Shift")`.
    pub(crate) fn click_with(&mut self, name: &str, keys: &str) -> UiResult {
        let (mods, _) = input::parse_chord(&format!("{keys}+A")).map_err(UiError)?;
        self.step("action", format!("{keys}-click “{name}”"), |s| {
            s.click_node(None, name, PointerButton::Primary, 1, mods)
        })
    }

    pub(crate) fn double_click(&mut self, name: &str) -> UiResult {
        self.step("action", format!("Double-click “{name}”"), |s| {
            s.click_node(None, name, PointerButton::Primary, 2, Modifiers::NONE)
        })
    }

    pub(crate) fn right_click(&mut self, name: &str) -> UiResult {
        self.step("action", format!("Right-click “{name}”"), |s| {
            s.click_node(None, name, PointerButton::Secondary, 1, Modifiers::NONE)
        })
    }

    /// Rest the pointer on a control; returns the tooltip it shows.
    pub(crate) fn hover(&mut self, name: &str) -> UiResult<Vec<String>> {
        self.step("action", format!("Hover over “{name}”"), |s| {
            let screen = s.screen();
            let n = s.aim(None, name, &|n: &Node| vec![n.rect.intersect(screen).center()])?;
            let p = s.target(&n);
            s.tooltip_at(p)
        })
    }

    /// Click at a point on screen (in points) that has no name, such as a
    /// spot on a curve. `what` says what is there, for the log.
    pub(crate) fn click_at(&mut self, p: Pos2, what: &str) -> UiResult {
        self.step("action", format!("Click {what}"), |s| {
            s.glide(p)?;
            s.click_here(PointerButton::Primary, 1)?;
            s.settle(20)
        })
    }

    /// Where a point inside a control is: (fx, fy) as fractions of its
    /// width and height from its top-left corner.
    pub(crate) fn point_in(&mut self, name: &str, fx: f32, fy: f32) -> UiResult<Pos2> {
        let n = self.find(None, name, 12)?;
        Ok(n.rect.min + egui::vec2(fx * n.rect.width(), fy * n.rect.height()))
    }

    /// Click inside a control at (dx, dy) points from its top-left corner:
    /// for parts of a control that have no name of their own (a layer
    /// row's eye). `what` says what is there, for the log.
    pub(crate) fn click_offset(&mut self, name: &str, dx: f32, dy: f32, what: &str) -> UiResult {
        self.step("action", format!("Click {what} in “{name}”"), |s| {
            s.aim(None, name, &|n: &Node| vec![n.rect.min + egui::vec2(dx, dy)])?;
            s.click_here(PointerButton::Primary, 1)?;
            s.settle(20)
        })
    }

    /// Click inside a control at a fraction (fx, fy) of its size.
    pub(crate) fn click_in(&mut self, name: &str, fx: f32, fy: f32, what: &str) -> UiResult {
        self.step("action", format!("Click {what} in “{name}”"), |s| {
            s.aim(None, name, &|n: &Node| {
                vec![n.rect.min + egui::vec2(fx * n.rect.width(), fy * n.rect.height())]
            })?;
            s.click_here(PointerButton::Primary, 1)?;
            s.settle(20)
        })
    }

    /// Drag inside a control between two fractions of its size.
    pub(crate) fn drag_in(&mut self, name: &str, from: (f32, f32), to: (f32, f32), what: &str) -> UiResult {
        self.step("action", format!("Drag {what} in “{name}”"), |s| {
            let at = |n: &Node, f: (f32, f32)| {
                n.rect.min + egui::vec2(f.0 * n.rect.width(), f.1 * n.rect.height())
            };
            let n = s.aim(None, name, &|n: &Node| vec![at(n, from), at(n, to)])?;
            s.drag_points(at(&n, from), at(&n, to), 12, Modifiers::NONE)
        })
    }

    /// Open a menu-bar menu and click through submenus by name:
    /// `menu("Layer > New fill layer > Solid color")`.
    pub(crate) fn menu(&mut self, path: &str) -> UiResult {
        let parts: Vec<String> = path
            .split(['>', '▸'])
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        let shown = parts.join(" ▸ ");
        self.step("action", format!("Click {shown}"), |s| {
            let (first, rest) = parts.split_first().ok_or("an empty menu path")?;
            // Open the menu bar menu, unless it is open already.
            let open = rest.first().is_some_and(|next| s.has_node(next));
            if !open {
                // The menu bar's button: the topmost of that name (a
                // history step can be called "Select" too).
                let found = s.find(Some(Role::Button), first, 12)?;
                let top = s
                    .tree
                    .matches(first, Some(Role::Button))
                    .into_iter()
                    .filter(|n| s.visible(n))
                    .min_by(|a, b| a.rect.min.y.total_cmp(&b.rect.min.y))
                    .cloned()
                    .unwrap_or(found);
                let p = s.target(&top);
                s.glide(p)?;
                s.click_here(PointerButton::Primary, 1)?;
            }
            for (i, part) in rest.iter().enumerate() {
                let n = s.find(Some(Role::Button), part, 20)?;
                if n.disabled {
                    let why = s.tooltip_at(s.target(&n))?;
                    return Err(UiError(format!("“{part}” is greyed out: {}", why.join(" "))));
                }
                let p = s.target(&n);
                if i + 1 == rest.len() {
                    s.glide(p)?;
                    s.click_here(PointerButton::Primary, 1)?;
                } else {
                    // A submenu opens on hover; travel along its row into it
                    // so the pointer never crosses a sibling item.
                    s.glide(p)?;
                    let next = &rest[i + 1];
                    let sub = s.find(Some(Role::Button), next, 20)?;
                    let entry = egui::pos2(sub.rect.left() + 14.0, p.y.clamp(sub.rect.top(), f32::MAX));
                    let row = egui::pos2(entry.x, p.y);
                    s.glide(row)?;
                }
            }
            s.settle(20)
        })
    }

    /// Press a key chord: `key("Cmd+Shift+N")`, `key("Enter")`.
    pub(crate) fn key(&mut self, chord: &str) -> UiResult {
        let (mods, key) = input::parse_chord(chord).map_err(UiError)?;
        let shown = input::chord_text(mods, key);
        self.step("action", format!("Press {shown}"), |s| {
            s.press_key(mods, key)?;
            s.settle(20)
        })
    }

    /// Type text into whatever has keyboard focus.
    pub(crate) fn type_text(&mut self, text: &str) -> UiResult {
        self.step("action", format!("Type “{text}”"), |s| {
            s.type_chars(text)?;
            s.settle(20)
        })
    }

    /// Click a named text or number field, replace its contents, Enter.
    pub(crate) fn set_field(&mut self, name: &str, text: &str) -> UiResult {
        self.step("action", format!("Set “{name}” to {text}"), |s| {
            let fields = [Role::TextInput, Role::MultilineTextInput, Role::SpinButton];
            let n = fields
                .iter()
                .find_map(|r| {
                    let m = s.tree.matches(name, Some(*r));
                    m.into_iter().filter(|n| s.visible(n)).last().cloned()
                })
                .map(Ok)
                .unwrap_or_else(|| s.find(None, name, 12))?;
            if n.disabled {
                return Err(UiError(format!("the field “{name}” is greyed out")));
            }
            let screen = s.screen();
            s.aim(Some(n.role), name, &|n: &Node| {
                vec![n.rect.intersect(screen).center()]
            })?;
            s.click_here(PointerButton::Primary, 1)?;
            s.frame()?;
            s.press_key(input::command(), egui::Key::A)?;
            s.type_chars(text)?;
            s.press_key(Modifiers::NONE, egui::Key::Enter)?;
            s.settle(20)
        })
    }

    /// Press at `from`, move to `to` over `steps` frames, release (points).
    pub(crate) fn drag(&mut self, from: Pos2, to: Pos2, steps: usize, keys: &str) -> UiResult {
        let mods = Self::mods(keys)?;
        self.step(
            "action",
            format!(
                "Drag from ({:.0}, {:.0}) to ({:.0}, {:.0})",
                from.x, from.y, to.x, to.y
            ),
            |s| s.drag_points(from, to, steps, mods),
        )
    }

    fn mods(keys: &str) -> UiResult<Modifiers> {
        if keys.trim().is_empty() {
            return Ok(Modifiers::NONE);
        }
        Ok(input::parse_chord(&format!("{keys}+A")).map_err(UiError)?.0)
    }

    fn drag_points(&mut self, from: Pos2, to: Pos2, steps: usize, mods: Modifiers) -> UiResult {
        self.glide(from)?;
        self.with_modifiers(mods, |s| {
            s.button(PointerButton::Primary, true)?;
            let steps = steps.max(2);
            for i in 1..=steps {
                let p = from + (to - from) * (i as f32 / steps as f32);
                s.pointer = p;
                s.events.push(Event::PointerMoved(p));
                s.frame()?;
            }
            s.frame()?;
            s.button(PointerButton::Primary, false)
        })?;
        self.settle(30)
    }

    /// Drag a slider's handle to `fraction` (0 to 1) of its track.
    pub(crate) fn drag_slider(&mut self, name: &str, fraction: f32) -> UiResult {
        self.step(
            "action",
            format!("Drag the “{name}” slider to {:.0}%", fraction * 100.0),
            |s| {
                let n = s.find(Some(Role::Slider), name, 12)?;
                if n.disabled {
                    return Err(UiError(format!("the slider “{name}” is greyed out")));
                }
                let at = |n: &Node, f: f32| {
                    let r = n.rect;
                    let radius = r.height() / 2.5;
                    egui::pos2(
                        r.left() + radius + f.clamp(0.0, 1.0) * (r.width() - 2.0 * radius),
                        r.center().y,
                    )
                };
                let now = match (n.numeric, n.min, n.max) {
                    (Some(v), Some(lo), Some(hi)) if hi > lo => ((v - lo) / (hi - lo)) as f32,
                    _ => 0.5,
                };
                let n = s.aim(Some(Role::Slider), name, &|n: &Node| {
                    vec![at(n, now), at(n, fraction)]
                })?;
                s.drag_points(at(&n, now), at(&n, fraction), 10, Modifiers::NONE)
            },
        )
    }

    /// Scroll with the wheel over a control (positive `dy` scrolls down).
    pub(crate) fn scroll(&mut self, name: &str, dy: f32) -> UiResult {
        self.step("action", format!("Scroll over “{name}”"), |s| {
            let n = s.find(None, name, 12)?;
            let p = s.target(&n);
            s.glide(p)?;
            s.wheel(dy)
        })
    }

    // ---- the canvas ---------------------------------------------------------

    /// Where document pixel (x, y) is on screen now, through the canvas's
    /// current zoom and pan, the way the canvas maps the pointer.
    pub(crate) fn doc_to_screen(&mut self, x: f32, y: f32) -> UiResult<Pos2> {
        let canvas = self.canvas_node()?;
        let (zoom, pan) = self.call(|st| (st.app.zoom, st.app.pan))?;
        Ok(canvas.rect.min + pan + egui::vec2(x, y) * zoom)
    }

    /// The canvas node of the last frame.
    fn canvas_node(&self) -> UiResult<Node> {
        // While text is edited on it, the canvas is a text field called
        // "Text on canvas".
        let mut found = self.tree.matches("Canvas", None);
        found.extend(self.tree.matches("Text on canvas", None));
        found
            .into_iter()
            .max_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
            .cloned()
            .ok_or_else(|| "no canvas on screen (is a document open?)".into())
    }

    /// Put the pointer on the canvas at `p`, failing when a floating bar
    /// or panel covers that spot.
    fn aim_canvas(&mut self, p: Pos2, doc: (f32, f32)) -> UiResult {
        self.glide(p)?;
        let canvas = self.canvas_node()?;
        if let Err(why) = self.on_node(&canvas)? {
            return Err(UiError(format!(
                "can't reach the canvas at document ({:.0}, {:.0}): {why}",
                doc.0, doc.1
            )));
        }
        Ok(())
    }

    /// Drag on the canvas between two document points (pixels), with
    /// `keys` held ("" for none, "Shift", "Alt").
    pub(crate) fn canvas_drag(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        steps: usize,
        keys: &str,
    ) -> UiResult {
        let mods = Self::mods(keys)?;
        self.step(
            "action",
            format!(
                "Drag on the canvas from ({:.0}, {:.0}) to ({:.0}, {:.0})",
                from.0, from.1, to.0, to.1
            ),
            |s| {
                let a = s.doc_to_screen(from.0, from.1)?;
                let b = s.doc_to_screen(to.0, to.1)?;
                s.aim_canvas(a, from)?;
                s.drag_points(a, b, steps, mods)
            },
        )
    }

    /// A drag on the canvas through several document points.
    pub(crate) fn canvas_stroke(&mut self, points: &[(f32, f32)], steps_per_segment: usize) -> UiResult {
        self.step(
            "action",
            format!("Drag a stroke through {} points on the canvas", points.len()),
            |s| {
                let pts: Vec<Pos2> = points
                    .iter()
                    .map(|(x, y)| s.doc_to_screen(*x, *y))
                    .collect::<UiResult<_>>()?;
                let (first, rest) = pts.split_first().ok_or("no points")?;
                s.aim_canvas(*first, points[0])?;
                s.button(PointerButton::Primary, true)?;
                let mut at = *first;
                for p in rest {
                    let n = steps_per_segment.max(1);
                    for i in 1..=n {
                        let q = at + (*p - at) * (i as f32 / n as f32);
                        s.pointer = q;
                        s.events.push(Event::PointerMoved(q));
                        s.frame()?;
                    }
                    at = *p;
                }
                s.frame()?;
                s.button(PointerButton::Primary, false)?;
                s.settle(30)
            },
        )
    }

    /// Click a document point on the canvas.
    pub(crate) fn canvas_click(&mut self, at: (f32, f32), keys: &str) -> UiResult {
        let mods = Self::mods(keys)?;
        self.step(
            "action",
            format!("Click the canvas at ({:.0}, {:.0})", at.0, at.1),
            |s| {
                let p = s.doc_to_screen(at.0, at.1)?;
                s.aim_canvas(p, at)?;
                s.with_modifiers(mods, |s| s.click_here(PointerButton::Primary, 1))?;
                s.settle(30)
            },
        )
    }

    /// Click a document point `count` times in quick succession (2: a
    /// double click, 3: a triple click), with `keys` held.
    pub(crate) fn canvas_multi_click(&mut self, at: (f32, f32), count: usize, keys: &str) -> UiResult {
        let mods = Self::mods(keys)?;
        self.step(
            "action",
            format!("Click the canvas {count} times at ({:.0}, {:.0})", at.0, at.1),
            |s| {
                let p = s.doc_to_screen(at.0, at.1)?;
                s.aim_canvas(p, at)?;
                s.with_modifiers(mods, |s| s.click_here(PointerButton::Primary, count))?;
                s.settle(30)
            },
        )
    }

    // ---- waiting -------------------------------------------------------------

    /// Run frames until the UI is still and background work (AI, export
    /// previews, canvas refresh) is done; fails after 30 s.
    pub(crate) fn wait_idle(&mut self) -> UiResult {
        self.step("wait", "Wait until Lumenply is idle".into(), |s| {
            s.settle_idle(Duration::from_secs(30))
        })
    }

    /// Let `n` frames pass with no input.
    pub(crate) fn wait_frames(&mut self, n: usize) -> UiResult {
        for _ in 0..n {
            self.frame()?;
        }
        Ok(())
    }

    // ---- system file panels ---------------------------------------------------

    fn await_dialog(&mut self) -> UiResult<DialogAbout> {
        if self.crashed.is_some() || self.quit {
            self.guard()?;
        }
        for _ in 0..20 {
            if let Some(d) = &self.dialog {
                return Ok(d.clone());
            }
            self.frame()?;
        }
        self.dialog
            .clone()
            .ok_or_else(|| UiError("no system file panel opened".into()))
    }

    fn answer(&mut self, kind: &'static str, path: Option<&Path>) -> UiResult {
        let shown = path.map(|p| match p.strip_prefix(&self.dir) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => p.display().to_string(),
        });
        let action = match shown {
            Some(p) if kind == "save" => format!("System panel: save as {p}"),
            Some(p) => format!("System panel: choose {p}"),
            None => "System panel: Cancel".into(),
        };
        self.step("system dialog", action, |s| {
            let d = s.await_dialog()?;
            if path.is_some() && d.kind != kind {
                return Err(UiError(format!(
                    "the app opened a {} panel (“{}”), not a {kind} panel",
                    d.kind, d.title
                )));
            }
            // Show the stand-in panel for a moment, then answer it.
            if let Some(f) = s.composed(None) {
                if let Some(v) = s.video.as_mut() {
                    v.hold(&f, s.opts.fps);
                }
            }
            s.ui.answer(path.map(Path::to_path_buf));
            s.dialog = None;
            s.receive_frame()?;
            s.settle_idle(Duration::from_secs(60))
        })
    }

    /// Answer the open panel the app is showing with `path` (a copy in the
    /// session's folder, never the user's own file).
    pub(crate) fn choose_file(&mut self, path: impl AsRef<Path>) -> UiResult {
        self.answer("open", Some(path.as_ref()))
    }

    /// Answer the save panel the app is showing with `path`.
    pub(crate) fn save_as(&mut self, path: impl AsRef<Path>) -> UiResult {
        self.answer("save", Some(path.as_ref()))
    }

    /// Cancel the system panel the app is showing.
    pub(crate) fn cancel_dialog(&mut self) -> UiResult {
        self.answer("open", None)
    }

    /// The system panel the app is waiting on, if any.
    pub(crate) fn open_dialog(&self) -> Option<&DialogAbout> {
        self.dialog.as_ref()
    }

    // ---- the clipboard -----------------------------------------------------------

    /// Text on the session's clipboard (what the app copied).
    pub(crate) fn clipboard_text(&self) -> &str {
        &self.clipboard_text
    }

    /// Put text on the session's clipboard, as if copied in another app.
    pub(crate) fn set_clipboard_text(&mut self, text: &str) {
        self.clipboard_text = text.to_string();
    }

    /// The image on the session's clipboard: (width, height).
    pub(crate) fn clipboard_image_size(&self) -> Option<(usize, usize)> {
        self.clipboard_image
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|(w, h, _)| (*w, *h)))
    }

    // ---- reading the app --------------------------------------------------------

    /// Read anything from the app (on its thread, between frames).
    pub(crate) fn app<R: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut App) -> R + Send + 'static,
    ) -> UiResult<R> {
        self.call(move |st| f(&mut st.app))
    }

    /// Read the open document.
    pub(crate) fn doc<R: Send + 'static>(
        &mut self,
        f: impl FnOnce(&Document) -> R + Send + 'static,
    ) -> UiResult<R> {
        self.call(move |st| f(st.app.editor.doc()))
    }

    /// Top-level layer names as the Layers panel lists them (top first).
    pub(crate) fn layer_names(&mut self) -> UiResult<Vec<String>> {
        self.doc(|d| d.layers().iter().rev().map(|l| l.name.clone()).collect())
    }

    /// The undo history's step names, oldest first.
    pub(crate) fn history(&mut self) -> UiResult<Vec<String>> {
        self.app(|a| a.editor.history().iter().map(|s| s.to_string()).collect())
    }

    /// The selection's bounds in document pixels (x, y, w, h): every
    /// pixel it covers at all.
    pub(crate) fn selection_bounds(&mut self) -> UiResult<Option<(i32, i32, u32, u32)>> {
        self.doc(|d| {
            d.selection.as_ref().map(|s| {
                let b = s.tight_bounds(d.canvas());
                (b.x, b.y, b.w, b.h)
            })
        })
    }

    /// The selection's coverage (0 to 1) at a document pixel.
    pub(crate) fn selection_at(&mut self, x: i32, y: i32) -> UiResult<f32> {
        self.doc(move |d| d.selection.as_ref().map_or(0.0, |s| s.value(x, y)))
    }

    /// The composite image at a document pixel as the screen shows it:
    /// straight sRGB, 0 to 255.
    pub(crate) fn pixel(&mut self, x: u32, y: u32) -> UiResult<[u8; 4]> {
        self.doc(move |d| {
            let flat = lumenply_render::composite_raster(d);
            let p = flat.get(x.min(flat.width - 1), y.min(flat.height - 1));
            let [r, g, b, a] = p.to_straight();
            [
                lumenply_io::linear_to_srgb(r),
                lumenply_io::linear_to_srgb(g),
                lumenply_io::linear_to_srgb(b),
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
    }

    /// A layer's own pixel (straight sRGB 0 to 255), found by name.
    pub(crate) fn layer_pixel(&mut self, layer: &str, x: i32, y: i32) -> UiResult<Option<[u8; 4]>> {
        let layer = layer.to_string();
        self.doc(move |d| {
            let l = find_layer(d, &layer)?;
            let p = l.pixels()?.get_pixel(x, y);
            let [r, g, b, a] = p.to_straight();
            Some([
                lumenply_io::linear_to_srgb(r),
                lumenply_io::linear_to_srgb(g),
                lumenply_io::linear_to_srgb(b),
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ])
        })
    }

    /// A layer's mask value (0 hides, 1 shows) at a document pixel.
    pub(crate) fn mask_at(&mut self, layer: &str, x: i32, y: i32) -> UiResult<Option<f32>> {
        let layer = layer.to_string();
        self.doc(move |d| {
            let l = find_layer(d, &layer)?;
            l.mask.as_ref().map(|m| m.value(x, y))
        })
    }

    /// Text the user can see: a control's name or value, or painted text.
    pub(crate) fn text_shown(&self, needle: &str) -> bool {
        let screen = self.screen();
        self.tree.texts().any(|t| t.contains(needle))
            || self
                .texts
                .iter()
                .any(|(t, r)| t.contains(needle) && r.intersects(screen))
    }

    /// Every piece of visible text containing `needle`.
    pub(crate) fn texts_matching(&self, needle: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .texts
            .iter()
            .filter(|(t, _)| t.contains(needle))
            .map(|(t, _)| t.clone())
            .collect();
        v.extend(
            self.tree
                .texts()
                .filter(|t| t.contains(needle))
                .map(str::to_string),
        );
        v.sort();
        v.dedup();
        v
    }

    // ---- checks --------------------------------------------------------------------

    /// Log a check of something already measured. Fails per `on_fail`.
    pub(crate) fn check(
        &mut self,
        what: &str,
        ok: bool,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> UiResult<bool> {
        let idx = self.begin("check", format!("Check: {what}"));
        let (expected, actual) = (expected.into(), actual.into());
        let outcome = if ok {
            Ok(())
        } else {
            Err(format!("expected {expected}, got {actual}"))
        };
        self.end(idx, outcome, Some(expected), Some(actual), true);
        if ok {
            return Ok(true);
        }
        self.failed_checks += 1;
        match self.opts.on_fail {
            OnFail::Continue => Ok(false),
            OnFail::Stop => Err(UiError(format!("check failed: {what}"))),
        }
    }

    /// Check two values are equal.
    pub(crate) fn check_eq<T: PartialEq + std::fmt::Debug>(
        &mut self,
        what: &str,
        actual: T,
        expected: T,
    ) -> UiResult<bool> {
        let ok = actual == expected;
        self.check(what, ok, format!("{expected:?}"), format!("{actual:?}"))
    }

    /// Check a control is on screen.
    pub(crate) fn expect_node(&mut self, name: &str) -> UiResult<bool> {
        let found = self.find(None, name, 12).ok();
        let actual = found
            .as_ref()
            .map_or("not on screen".to_string(), |n| n.describe());
        self.check(
            &format!("“{name}” is shown"),
            found.is_some(),
            "on screen",
            actual,
        )
    }

    /// Check a control is on screen and enabled.
    pub(crate) fn expect_enabled(&mut self, name: &str) -> UiResult<bool> {
        let found = self.find(None, name, 12).ok();
        let ok = found.as_ref().is_some_and(|n| !n.disabled);
        let actual = found
            .as_ref()
            .map_or("not on screen".to_string(), |n| n.describe());
        self.check(&format!("“{name}” can be used"), ok, "enabled", actual)
    }

    /// Check a control is greyed out, and that its tooltip gives a reason
    /// containing `reason` (empty: any reason, or none).
    pub(crate) fn expect_disabled(&mut self, name: &str, reason: &str) -> UiResult<bool> {
        let found = self.find(None, name, 12).ok();
        let (ok, actual) = match &found {
            None => (false, "not on screen".to_string()),
            Some(n) if !n.disabled => (false, "enabled".to_string()),
            Some(n) => {
                let why = self.tooltip_at(self.target(n))?.join(" ");
                (why.contains(reason), format!("disabled; tooltip “{why}”"))
            }
        };
        let expected = if reason.is_empty() {
            "disabled".to_string()
        } else {
            format!("disabled, saying “{reason}”")
        };
        self.check(&format!("“{name}” is greyed out"), ok, expected, actual)
    }

    /// Check some text is visible (status bar, dialog, label, canvas).
    pub(crate) fn expect_text(&mut self, needle: &str) -> UiResult<bool> {
        // Text can arrive a frame or two after the action that caused it.
        for _ in 0..6 {
            if self.text_shown(needle) {
                break;
            }
            self.frame()?;
        }
        let ok = self.text_shown(needle);
        let actual = if ok {
            self.texts_matching(needle).join(" | ")
        } else {
            "not shown".into()
        };
        self.check(&format!("“{needle}” is shown"), ok, "visible", actual)
    }

    /// Check the open document with a closure returning (passed, what it saw).
    pub(crate) fn expect_doc(
        &mut self,
        what: &str,
        expected: &str,
        f: impl FnOnce(&Document) -> (bool, String) + Send + 'static,
    ) -> UiResult<bool> {
        let (ok, actual) = self.doc(f)?;
        self.check(what, ok, expected, actual)
    }
}

/// The visible node named `name` in one frame's tree: of `role` if given;
/// among several, the one drawn last (on top). An error when the name is
/// shared by different kinds of control and no role says which.
fn pick(
    tree: &Tree,
    role: Option<Role>,
    name: &str,
    screen: egui::Rect,
    within: Option<egui::Rect>,
) -> Result<Option<Node>, String> {
    let visible = |n: &&Node| {
        let r = n.rect.intersect(screen);
        r.width() >= 1.0 && r.height() >= 1.0 && within.is_none_or(|w| w.contains(n.rect.center()))
    };
    let shown: Vec<&Node> = tree.matches(name, role).into_iter().filter(visible).collect();
    if shown.is_empty() {
        return Ok(None);
    }
    let roles = |v: &[&Node]| {
        v.iter()
            .map(|n| n.role)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    if role.is_none() && roles(&shown) > 1 {
        // A control wins over static text of the same name.
        let controls: Vec<&Node> = shown
            .iter()
            .copied()
            .filter(|n| !matches!(n.role, Role::Label | Role::InlineTextBox))
            .collect();
        if !controls.is_empty() && roles(&controls) == 1 {
            return Ok(controls.last().map(|n| (*n).clone()));
        }
        let list: Vec<String> = shown.iter().map(|n| n.describe()).collect();
        return Err(format!(
            "“{name}” names {} different controls; say which with a role: {}",
            shown.len(),
            list.join("; ")
        ));
    }
    Ok(shown.last().map(|n| (*n).clone()))
}

/// The first layer named `name`, groups included, in panel order.
pub(crate) fn find_layer<'a>(doc: &'a Document, name: &str) -> Option<&'a lumenply_doc::Layer> {
    let mut id = None;
    doc.for_each_layer(|l| {
        if l.name == name {
            id = Some(l.id);
        }
    });
    id.and_then(|i| doc.layer(i))
}

fn dialog_lines(d: &DialogAbout) -> Vec<String> {
    let mut v = vec![format!(
        "{} — {}",
        if d.kind == "save" { "Save" } else { "Open" },
        d.title
    )];
    if let Some(n) = &d.file_name {
        v.push(format!("Suggested name: {n}"));
    }
    if !d.filters.is_empty() {
        let f: Vec<String> = d
            .filters
            .iter()
            .take(3)
            .map(|(name, exts)| format!("{name} (.{})", exts.join(", .")))
            .collect();
        v.push(f.join(" · "));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_drives_the_real_ui_through_input_alone() {
        let out = std::env::temp_dir().join("lumenply-uitest-unit").join("session");
        let mut opts = Options::new(&out);
        opts.record = false; // no GPU needed
        let mut s = Session::start("unit-session", opts).expect("launch");
        assert!(s.text_shown("No document open"));
        s.click("New image…").unwrap();
        s.set_field("Width", "320").unwrap();
        s.set_field("Height", "200").unwrap();
        s.click("Create").unwrap();
        assert_eq!(s.doc(|d| (d.width, d.height)).unwrap(), (320, 200));
        assert_eq!(s.layer_names().unwrap(), vec!["Background".to_string()]);
        // A file panel parks the frame until the scenario answers it.
        s.menu("File > Open...").unwrap();
        assert_eq!(s.open_dialog().map(|d| d.kind), Some("open"));
        assert!(s.wait_frames(1).is_err(), "no input while a system panel is up");
        s.cancel_dialog().unwrap();
        assert!(s.open_dialog().is_none());
        assert!(s.click("No such control").is_err());
        let summary = s.finish(Ok(()));
        assert!(!summary.passed, "the failed click is logged");
        assert_eq!(summary.failed.len(), 1, "{:?}", summary.failed);
        let log = std::fs::read_to_string(summary.log).unwrap();
        assert!(
            log.contains("\"system dialog\""),
            "the panel is logged as a system dialog"
        );
    }
}
