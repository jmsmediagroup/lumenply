//! Desktop editor shell built on egui.
//!
//! A thin layer over `lumenply_core::Editor`: every edit is a `Command`, so undo,
//! history and (later) scripting behave exactly as in the headless CLI.
//! Compositing is still the CPU reference renderer; redraws are limited to
//! the area a command reports as affected, which keeps brushing responsive.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::OnceLock;

use eframe::egui::{
    self, Align2, Color32, FontId, Key, Pos2, RichText, Sense, Shape, Stroke, TextureHandle, Vec2,
};
use lumenply_core::commands::*;
use lumenply_core::{Command, Editor};
use lumenply_doc::{
    Adjustment, BlendMode, CombineOp, Document, Filter, Layer, LayerContent, LayerId, Selection, TextAlign,
    TextLayer,
};
use lumenply_io::project;
use lumenply_tiles::{Affine, Raster, Rect};

mod adjust_ui;
mod brand;
mod camera_raw;
mod canvas;
mod clipboard;
mod color_picker;
mod crop;
mod debug;
mod demo;
mod dialogs;
mod export_as;
mod gradient_ui;
mod guides;
mod histogram;
mod history;
mod layer_actions;
mod layers;
mod liquify;
mod macos_open;
mod menu;
mod options_bar;
mod palette;
mod pen;
mod properties;
mod quick_select_tool;
#[cfg(test)]
mod select_fill_tests;
mod session;
mod shape_tool;
mod smart_contents;
mod start;
mod status;
mod text_ui;
mod theme;
mod tools;

pub(crate) use canvas::*;
pub(crate) use dialogs::Dialog;
pub(crate) use palette::Palette;
pub(crate) use theme::*;
pub(crate) use tools::Tool;

fn main() -> Result<(), eframe::Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--write-icon PATH SIZE`: the app icon as a PNG (packaging builds the
    // macOS .icns from these), then exit.
    if let Some(i) = args.iter().position(|a| a == "--write-icon") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("icon.png");
        let size: usize = args
            .get(i + 2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024)
            .clamp(16, 2048);
        // macOS icon grid: the tile takes 824/1024 of the canvas, centred,
        // with transparent margins like every other Dock icon.
        let inner = (size * 824 / 1024).max(1);
        let tile = image::RgbaImage::from_raw(inner as u32, inner as u32, brand::icon_rgba(inner))
            .expect("icon buffer matches its size");
        let mut img = image::RgbaImage::new(size as u32, size as u32);
        let off = ((size - inner) / 2) as i64;
        image::imageops::overlay(&mut img, &tile, off, off);
        if let Err(e) = img.save_with_format(path, image::ImageFormat::Png) {
            eprintln!("could not write {path}: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(std::sync::Arc::new(egui::IconData {
                rgba: brand::icon_rgba(256),
                width: 256,
                height: 256,
            }))
            .with_inner_size(window_size(&args))
            .with_min_inner_size([900.0, 600.0])
            .with_title("Lumenply")
            // A headless screenshot run must not take keyboard focus from
            // whatever the user is typing into meanwhile.
            .with_active(!args.iter().any(|a| a == "--screenshot")),
        ..Default::default()
    };
    macos_open::install();
    eframe::run_native(
        "Lumenply",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, &args)))),
    )
}

impl App {
    /// Files dragged onto the window: projects open, images land as layers
    /// in the current document (undoable), with a hint while hovering. With
    /// no document open (the welcome screen) every file opens in a tab.
    fn handle_file_drop(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.screen_rect();
            let p = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop-hint"),
            ));
            p.rect_filled(screen, 0.0, Color32::from_black_alpha(120));
            p.text(
                screen.center(),
                Align2::CENTER_CENTER,
                if self.no_doc {
                    "Drop to open"
                } else {
                    "Drop to open — images are placed as a new layer"
                },
                FontId::proportional(18.0),
                TEXT,
            );
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        let place = !self.no_doc && self.editor.doc().layer_count() > 0;
        for path in dropped {
            let p = path.to_string_lossy().into_owned();
            if place && is_image_path(&p) {
                self.place_image(&p);
            } else {
                self.open_path(&p);
            }
        }
    }

    /// `--screenshot`: wait a few frames for everything to upload, ask the
    /// backend for a frame grab, save it, quit. Lets the UI be inspected
    /// without macOS screen-recording permission.
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        // `--screenshot-do a,b,...`: palette action ids run once before the
        // capture, so UI states behind a click can be verified headlessly.
        // Two debug-only tokens: `select-pixel` activates the topmost pixel
        // layer; `debug-bend` drags an inner warp point.
        if self.shot.is_some() && !self.shot_do.is_empty() {
            for act in std::mem::take(&mut self.shot_do) {
                match act.as_str() {
                    "select-pixel" => {
                        let id = self
                            .editor
                            .doc()
                            .layers()
                            .iter()
                            .rev()
                            .find(|l| l.pixels().is_some())
                            .map(|l| l.id);
                        self.set_active(id);
                    }
                    "debug-bend" => {
                        if let Some(mut x) = self.xform.clone() {
                            if let Some(w) = x.warp.as_mut() {
                                w[5].0 += 90.0;
                                w[5].1 -= 60.0;
                            }
                            self.preview_xform(ctx, &mut x);
                            self.xform = Some(x);
                        }
                    }
                    other => {
                        if !self.debug_token(ctx, other) {
                            self.run_menu_action(other);
                        }
                    }
                }
            }
        }
        let Some((path, frames_left)) = &mut self.shot else {
            return;
        };
        let got = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = got {
            let [w, h] = img.size;
            let mut buf = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                buf.extend_from_slice(&p.to_array());
            }
            let out = image::RgbaImage::from_raw(w as u32, h as u32, buf).expect("buffer matches dimensions");
            let r = out.save_with_format(&*path, image::ImageFormat::Png);
            eprintln!("screenshot {:?}: {:?}", path, r.err());
            std::process::exit(0);
        }
        if *frames_left == 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        } else {
            *frames_left -= 1;
        }
        ctx.request_repaint();
    }
}

/// What Delete/Backspace does with this tool active: clear the layer's
/// pixels, except where the tool itself uses the key (the pen deletes the
/// selected path node), so one keystroke never does both.
fn delete_key_action(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::Pen | Tool::Crop => None,
        _ => Some("clear"),
    }
}

/// `--window-size WxH` (debug: check narrow layouts headlessly), else the
/// default 1600×1000.
fn window_size(args: &[String]) -> [f32; 2] {
    args.iter()
        .position(|a| a == "--window-size")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])
        })
        .filter(|[w, h]| *w >= 200.0 && *h >= 200.0)
        .unwrap_or([1600.0, 1000.0])
}

fn is_image_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".tif", ".tiff", ".webp", ".exr"]
        .iter()
        .any(|e| lower.ends_with(e))
        || lumenply_io::raw::is_raw(p)
}

fn is_ora_path(p: &str) -> bool {
    p.to_ascii_lowercase().ends_with(".ora")
}

fn is_psd_path(p: &str) -> bool {
    let p = p.to_ascii_lowercase();
    p.ends_with(".psd") || p.ends_with(".psb")
}

// ---- small enums ---------------------------------------------------------------

// ---- the app -----------------------------------------------------------------------

struct App {
    editor: Editor,
    tool: Tool,
    active: Option<LayerId>,
    selected: Vec<LayerId>,
    editing_mask: bool,
    path: Option<PathBuf>,

    // tool options
    brush: Brush,
    brush_rgb: [f32; 3],
    bg_rgb: [f32; 3],
    select_op: CombineOp,
    feather: f32,
    tolerance: f32,
    contiguous: bool,
    sample_merged: bool,
    text_size: f32,
    text_bold: bool,
    text_italic: bool,
    text_font: String,
    text_align: TextAlign,
    /// "New text" pressed: the next Text-tool click starts a new layer.
    text_new_armed: bool,
    /// Edit ▸ Copy's pixels at full precision.
    clip: Option<clipboard::Clip>,
    /// Quick Selection (the Wand tool's sibling mode) and its stroke.
    quick: quick_select_tool::QuickSelectState,
    /// Identifies the live document across tab switches (contents tabs
    /// save back to their parent by key).
    doc_key: u64,
    next_doc_key: u64,
    /// Set in a smart object's contents tab: Save writes back there.
    smart_link: Option<smart_contents::SmartLink>,
    /// File ▸ Export ▸ Export As…, while open (it replaces the editor UI).
    export_as: Option<Box<export_as::ExportAsState>>,
    /// Filter > Liquify's workspace, while open (it replaces the editor UI).
    liquify: Option<Box<liquify::LiquifyState>>,
    /// The Camera Raw develop workspace, while a RAW file is being opened.
    camera_raw: Option<Box<camera_raw::CameraRawState>>,
    /// Clone source point (document space), and whether the next click picks it.
    clone_source: Option<(f32, f32)>,
    clone_picking: bool,
    /// Offset locked in when a clone stroke starts.
    clone_offset: (i32, i32),

    // view
    zoom: f32,
    pan: Vec2,
    view_cmd: Option<ViewCmd>,
    canvas_tex: Option<TextureHandle>,
    overlay_tex: Option<TextureHandle>,
    thumbs: HashMap<LayerId, TextureHandle>,
    mask_thumbs: HashMap<LayerId, TextureHandle>,
    dirty: bool,
    dirty_rect: Option<Rect>,
    last_flat: Option<Raster>,

    // interaction
    drag: Option<DragKind>,
    drag_start: Option<Pos2>,
    stroke: Vec<StrokePoint>,
    /// Points already painted onto the preview texture this stroke.
    stroke_drawn: usize,
    /// Points of a lasso in progress (document space).
    lasso: Vec<(f32, f32)>,
    curve_drag: Option<usize>,
    cursor_doc: Option<(i32, i32)>,
    move_offset: (i32, i32),
    renaming: Option<(LayerId, String)>,
    xform: Option<Xform>,
    xform_scale: f32,
    xform_angle: f32,
    cb_tone: usize,
    /// Levels channel being edited: 0 master, 1-3 = R, G, B.
    levels_ch: usize,

    dialog: Option<Dialog>,
    filter_previewed: bool,
    status: String,
    /// History length at the last save, for the unsaved-changes dot.
    saved_rev: usize,
    /// One thumbnail per history step (step 0 is the opened state).
    hist_thumbs: Vec<egui::TextureHandle>,
    /// Open command palette (Ctrl+K).
    palette: Option<Palette>,
    /// Set once the user confirms quitting with unsaved changes.
    allow_close: bool,
    /// Composite cache for everything below the layer being edited.
    below: lumenply_render::BelowCache,
    /// When the last autosave backup was written (or the session began).
    last_autosave: std::time::Instant,
    /// Recently opened or saved files, newest first.
    recent: Vec<String>,
    /// Luminance histogram of the composite, updated on refresh.
    histogram: [u32; histogram::BINS],
    /// User preferences (undo caps, canvas colour, autosave interval).
    prefs: session::Prefs,
    /// Pen: the last subpath is still being extended.
    pen_open: bool,
    /// Pen: dragging out the handles of the just-placed node.
    pen_dragging: bool,
    /// Pen: an existing anchor or handle being dragged.
    pen_hit: Option<PenHit>,
    /// Pen: the selected node (its handles are shown and grabbable).
    pen_sel: Option<(usize, usize)>,
    /// Healing brush: true = spot mode (no texture source needed).
    heal_spot: bool,
    /// Quick-mask mode: paint the selection itself under a red overlay.
    quick_mask: bool,
    /// Selection boundary pixels for the animated marching ants.
    sel_points: Vec<(i32, i32)>,
    /// Layer row being dragged to a new position in the panel.
    layer_drag: Option<LayerId>,
    /// Startup splash: visible until this instant (None = done/skipped).
    splash_until: Option<std::time::Instant>,
    splash_tex: Option<egui::TextureHandle>,
    /// Last window title pushed to the OS, to avoid resending each frame.
    last_title: String,
    /// Debug: save a screenshot of the window here after a few frames,
    /// then exit (`--screenshot path.png`). Used to verify the UI headlessly.
    shot: Option<(PathBuf, u32)>,
    /// Debug: actions to run before the screenshot (`--screenshot-do`).
    shot_do: Vec<String>,
    /// Documents open in other tabs, in display order with the live
    /// document occupying slot `cur_tab` (its state lives in the fields
    /// above, not in this list).
    tabs: Vec<DocTab>,
    cur_tab: usize,
    /// Tab label while the live document has no file path.
    untitled: String,
    /// No document is open: the welcome screen replaces the editor and
    /// `editor` is an empty placeholder that nothing may edit (see start.rs).
    no_doc: bool,
    /// The welcome screen's demo thumbnail, decoded on first show.
    start_thumb: Option<TextureHandle>,
    /// The Crop tool's frame and options (crop.rs).
    crop: crop::CropTool,
    /// Rulers, guides, grid and snapping state (guides.rs).
    aids: guides::ViewAids,
    /// The Shape tool's options and drag (shape_tool.rs).
    shape: shape_tool::ShapeTool,
    /// The Gradient tool's options, popover and drag (gradient_ui.rs).
    gradient: gradient_ui::GradientTool,
}

/// A document parked in an inactive tab: its editor plus the per-document
/// state that would otherwise live in the `App` fields.
struct DocTab {
    doc_key: u64,
    smart_link: Option<smart_contents::SmartLink>,
    editor: Editor,
    path: Option<PathBuf>,
    saved_rev: usize,
    zoom: f32,
    pan: Vec2,
    active: Option<LayerId>,
    hist_thumbs: Vec<egui::TextureHandle>,
    untitled: String,
}

impl DocTab {
    fn title(&self) -> String {
        self.path
            .as_ref()
            .map(|p| file_name(&p.to_string_lossy()))
            .unwrap_or_else(|| self.untitled.clone())
    }

    fn unsaved(&self) -> bool {
        self.editor.history().len() != self.saved_rev
    }
}

impl App {
    /// `lumenply-app [--demo] [file.lumen | image.png | poster.psd ...] [--place image.png]...`
    /// With nothing to open, the app starts on the welcome screen.
    fn new(cc: &eframe::CreationContext<'_>, args: &[String]) -> Self {
        theme::install(&cc.egui_ctx);
        pen::install();
        clipboard::install();
        macos_open::set_waker(&cc.egui_ctx);
        Self::launch(args)
    }

    /// The app state for a command line, without a window (tests use it).
    fn launch(args: &[String]) -> Self {
        let launch = start::parse_launch_args(args);
        let mut app = App {
            editor: Editor::new(Document::new(1, 1)),
            tool: Tool::Brush,
            active: None,
            selected: Vec::new(),
            editing_mask: false,
            path: None,
            brush: Brush {
                radius: 14.0,
                hardness: 0.7,
                color: [0.0, 0.0, 0.0, 1.0],
                spacing: 0.12,
                jitter: 0.0,
                mode: BrushMode::Paint,
            },
            brush_rgb: [0.10, 0.18, 0.55],
            bg_rgb: [1.0, 1.0, 1.0],
            select_op: CombineOp::Replace,
            feather: 12.0,
            tolerance: 0.12,
            contiguous: true,
            sample_merged: false,
            text_size: 72.0,
            text_bold: false,
            text_italic: false,
            text_font: String::new(),
            text_align: TextAlign::Left,
            text_new_armed: false,
            liquify: None,
            doc_key: 0,
            next_doc_key: 0,
            smart_link: None,
            export_as: None,
            quick: Default::default(),
            clip: None,
            camera_raw: None,
            clone_source: None,
            clone_picking: true,
            clone_offset: (0, 0),
            zoom: 1.0,
            pan: Vec2::ZERO,
            view_cmd: Some(ViewCmd::Fit),
            canvas_tex: None,
            overlay_tex: None,
            thumbs: HashMap::new(),
            mask_thumbs: HashMap::new(),
            dirty: true,
            dirty_rect: None,
            last_flat: None,
            drag: None,
            drag_start: None,
            stroke: Vec::new(),
            stroke_drawn: 0,
            lasso: Vec::new(),
            curve_drag: None,
            cursor_doc: None,
            move_offset: (0, 0),
            renaming: None,
            xform: None,
            xform_scale: 100.0,
            xform_angle: 0.0,
            cb_tone: 1,
            levels_ch: 0,
            dialog: None,
            saved_rev: 0,
            hist_thumbs: Vec::new(),
            palette: None,
            allow_close: false,
            below: lumenply_render::BelowCache::new(),
            last_autosave: std::time::Instant::now(),
            recent: session::load_recent(),
            histogram: [0; histogram::BINS],
            prefs: session::Prefs::load(),
            pen_open: false,
            pen_dragging: false,
            pen_hit: None,
            pen_sel: None,
            heal_spot: true,
            quick_mask: false,
            sel_points: Vec::new(),
            layer_drag: None,
            splash_until: {
                // LUMENPLY_SPLASH_MS overrides (0 disables); screenshot
                // runs skip the splash unless the override asks for it.
                let ms = std::env::var("LUMENPLY_SPLASH_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(if args.iter().any(|a| a == "--screenshot") {
                        0
                    } else {
                        1400
                    });
                (ms > 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms))
            },
            splash_tex: None,
            last_title: String::new(),
            shot: args
                .iter()
                .position(|a| a == "--screenshot")
                .and_then(|i| args.get(i + 1))
                .map(|p| (PathBuf::from(p), 6)),
            shot_do: args
                .iter()
                .position(|a| a == "--screenshot-do")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            filter_previewed: false,
            status: String::from("Ready"),
            tabs: Vec::new(),
            cur_tab: 0,
            untitled: String::new(),
            no_doc: true,
            start_thumb: None,
            crop: crop::CropTool::default(),
            aids: guides::ViewAids::default(),
            shape: Default::default(),
            gradient: Default::default(),
        };
        // Everything opens through the same paths as File → Open, so a
        // file that fails to load leaves its error on the welcome screen.
        if launch.demo {
            app.open_demo();
        }
        for f in &launch.files {
            app.open_path(f);
        }
        for p in &launch.places {
            app.place_image(p);
        }
        if session::autosave_file().is_some_and(|p| p.exists()) {
            app.dialog = Some(Dialog::Recover);
        }
        app
    }

    // ---- document plumbing -------------------------------------------------

    fn set_active(&mut self, id: Option<LayerId>) {
        self.active = id;
        self.selected = id.into_iter().collect();
        self.editing_mask = false;
        // Dropping a live transform must also drop its canvas preview, or
        // the stale transformed texture lingers over the untouched document.
        if self.xform.take().is_some() {
            self.mark(None);
        }
    }

    /// Drop any in-progress canvas interaction (drag, stroke, lasso, clone
    /// source, free transform). Called when the document is replaced.
    fn cancel_interaction(&mut self) {
        self.drag = None;
        self.pen_open = false;
        self.pen_dragging = false;
        self.pen_hit = None;
        self.pen_sel = None;
        self.drag_start = None;
        self.stroke.clear();
        self.lasso.clear();
        self.clone_source = None;
        self.clone_picking = true;
        self.clone_offset = (0, 0);
        self.move_offset = (0, 0);
        self.crop.frame = None;
        self.aids = guides::ViewAids::default();
        if self.xform.take().is_some() {
            self.mark(None);
        }
    }

    fn select_top(&mut self) {
        let top = self.editor.doc().layers().last().map(|l| l.id);
        self.set_active(top);
    }

    fn fix_active(&mut self) {
        let ok = self
            .active
            .is_some_and(|id| self.editor.doc().layer(id).is_some());
        if !ok {
            self.select_top();
        }
        self.selected.retain(|id| self.editor.doc().layer(*id).is_some());
    }

    fn mark(&mut self, rect: Option<Rect>) {
        self.dirty_rect = if !self.dirty {
            rect
        } else {
            match (self.dirty_rect, rect) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                _ => None,
            }
        };
        self.dirty = true;
    }

    fn run(&mut self, cmd: &dyn Command) {
        if self.no_doc {
            return; // the welcome screen's placeholder is never edited
        }
        match self.editor.execute(cmd) {
            Ok(()) => {
                let r = self.editor.last_affected();
                let t = self.editor.last_target_layer();
                self.below.note_change(self.editor.doc(), t);
                self.mark(r);
                self.fix_active();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn run_coalescing(&mut self, cmd: &dyn Command, key: &str) {
        if self.no_doc {
            return;
        }
        match self.editor.execute_coalescing(cmd, key) {
            Ok(()) => {
                let r = self.editor.last_affected();
                let t = self.editor.last_target_layer();
                self.below.note_change(self.editor.doc(), t);
                self.mark(r);
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Replace the live tab's document (startup, crash recovery).
    fn set_doc(&mut self, editor: Editor, path: Option<PathBuf>) {
        self.no_doc = false;
        self.doc_key = self.alloc_doc_key();
        self.smart_link = None;
        self.editor = editor;
        self.prefs.apply(&mut self.editor);
        self.path = path;
        self.saved_rev = self.editor.history().len();
        self.hist_thumbs.clear();
        self.below = lumenply_render::BelowCache::new();
        self.cancel_interaction();
        self.select_top();
        self.mark(None);
        self.view_cmd = Some(ViewCmd::Fit);
    }

    // ---- document tabs -------------------------------------------------

    /// Move the live document's state out into a parked tab.
    fn park_live(&mut self) -> DocTab {
        DocTab {
            doc_key: self.doc_key,
            smart_link: self.smart_link.take(),
            editor: std::mem::replace(&mut self.editor, Editor::new(Document::new(1, 1))),
            path: self.path.take(),
            saved_rev: self.saved_rev,
            zoom: self.zoom,
            pan: self.pan,
            active: self.active,
            hist_thumbs: std::mem::take(&mut self.hist_thumbs),
            untitled: self.untitled.clone(),
        }
    }

    /// Make a parked tab the live document, restoring its view.
    fn load_tab(&mut self, t: DocTab) {
        self.doc_key = t.doc_key;
        self.smart_link = t.smart_link;
        self.editor = t.editor;
        self.path = t.path;
        self.saved_rev = t.saved_rev;
        self.hist_thumbs = t.hist_thumbs;
        self.untitled = t.untitled;
        self.below = lumenply_render::BelowCache::new();
        self.cancel_interaction();
        self.set_active(t.active);
        self.fix_active();
        self.zoom = t.zoom;
        self.pan = t.pan;
        self.mark(None);
    }

    /// Display-order titles with their unsaved flags, live tab included.
    pub(crate) fn tab_infos(&self) -> Vec<(String, bool)> {
        if self.no_doc {
            return Vec::new();
        }
        let live_title = self
            .path
            .as_ref()
            .map(|p| file_name(&p.to_string_lossy()))
            .unwrap_or_else(|| self.untitled.clone());
        let live_unsaved = self.editor.history().len() != self.saved_rev;
        let mut out: Vec<(String, bool)> = Vec::with_capacity(self.tabs.len() + 1);
        for (i, t) in self.tabs.iter().enumerate() {
            if i == self.cur_tab {
                out.push((live_title.clone(), live_unsaved));
            }
            out.push((t.title(), t.unsaved()));
        }
        if self.cur_tab >= self.tabs.len() {
            out.push((live_title, live_unsaved));
        }
        out
    }

    pub(crate) fn switch_tab(&mut self, i: usize) {
        if i == self.cur_tab || i > self.tabs.len() {
            return;
        }
        let parked = self.park_live();
        self.tabs.insert(self.cur_tab, parked);
        let t = self.tabs.remove(i);
        self.cur_tab = i;
        self.load_tab(t);
    }

    /// Open a document in a new tab at the end of the strip.
    pub(crate) fn open_in_new_tab(&mut self, editor: Editor, path: Option<PathBuf>) {
        // From the welcome screen there is no live document to park.
        if !self.no_doc {
            let parked = self.park_live();
            self.tabs.insert(self.cur_tab, parked);
        }
        self.cur_tab = self.tabs.len();
        self.untitled = self.next_untitled();
        self.set_doc(editor, path);
    }

    /// "Untitled-N" one past the highest N among the open, never-saved
    /// tabs. Imports and the demo relabel their tab, so they never use up
    /// a number (a counter would skip after them).
    fn next_untitled(&self) -> String {
        let n = |s: &str| s.strip_prefix("Untitled-").and_then(|n| n.parse::<usize>().ok());
        let live = (!self.no_doc && self.path.is_none())
            .then(|| n(&self.untitled))
            .flatten();
        let max = self
            .tabs
            .iter()
            .filter(|t| t.path.is_none())
            .filter_map(|t| n(&t.untitled))
            .chain(live)
            .max()
            .unwrap_or(0);
        format!("Untitled-{}", max + 1)
    }

    /// If `path` is already open in some tab, switch to it.
    fn focus_tab_with_path(&mut self, path: &str) -> bool {
        let wanted = PathBuf::from(path);
        if self.path.as_ref() == Some(&wanted) {
            return true;
        }
        let hit = self.tabs.iter().position(|t| t.path.as_ref() == Some(&wanted));
        if let Some(idx) = hit {
            // Parked index -> display index (the live tab shifts by one).
            let display = if idx < self.cur_tab { idx } else { idx + 1 };
            self.switch_tab(display);
            return true;
        }
        false
    }

    /// Close a tab by display index; asks about unsaved changes first.
    pub(crate) fn close_tab(&mut self, i: usize) {
        let unsaved = if i == self.cur_tab {
            self.editor.history().len() != self.saved_rev
        } else {
            let idx = if i < self.cur_tab { i } else { i - 1 };
            self.tabs.get(idx).is_some_and(|t| t.unsaved())
        };
        if unsaved {
            // Bring the tab forward so "Save" acts on what the user sees.
            if i != self.cur_tab {
                self.switch_tab(i);
            }
            self.dialog = Some(Dialog::ConfirmCloseTab(self.cur_tab));
        } else {
            self.force_close_tab(i);
        }
    }

    pub(crate) fn force_close_tab(&mut self, i: usize) {
        if i == self.cur_tab {
            if self.tabs.is_empty() {
                // The last tab closes onto the welcome screen.
                self.show_welcome();
                return;
            }
            // Load a neighbour; the parked list already excludes the
            // closing live tab, so its indices need no adjustment.
            let idx = i.min(self.tabs.len() - 1);
            let t = self.tabs.remove(idx);
            self.cur_tab = idx;
            self.load_tab(t);
        } else if i <= self.tabs.len() {
            let idx = if i < self.cur_tab { i } else { i - 1 };
            self.tabs.remove(idx);
            if i < self.cur_tab {
                self.cur_tab -= 1;
            }
        }
    }

    /// True when any open tab has unsaved changes.
    fn any_unsaved(&self) -> bool {
        self.editor.history().len() != self.saved_rev || self.tabs.iter().any(|t| t.unsaved())
    }

    fn active_layer(&self) -> Option<&Layer> {
        self.active.and_then(|id| self.editor.doc().layer(id))
    }

    fn active_is_pixel(&self) -> bool {
        self.active_layer().is_some_and(|l| l.pixels().is_some())
    }

    fn active_is_group(&self) -> bool {
        self.active_layer().is_some_and(|l| l.children().is_some())
    }

    fn active_text(&self) -> Option<TextLayer> {
        self.active_layer().and_then(|l| l.text_layer()).cloned()
    }

    fn active_is_text(&self) -> bool {
        self.active_text().is_some()
    }

    fn active_has_mask(&self) -> bool {
        self.active_layer().is_some_and(|l| l.mask.is_some())
    }

    fn make_brush(&self) -> Brush {
        let mut b = self.brush;
        b.color = linear_rgba(self.brush_rgb, self.brush.color[3].max(0.0));
        b.mode = match self.tool {
            Tool::Eraser => BrushMode::Erase,
            // The Brush tool keeps its chosen mode (Paint / Dodge / Burn).
            Tool::Brush => self.brush.mode,
            _ => BrushMode::Paint,
        };
        b
    }

    fn selection_op(&self, ctx: &egui::Context) -> CombineOp {
        let mods = ctx.input(|i| i.modifiers);
        if mods.shift && mods.alt {
            CombineOp::Intersect
        } else if mods.shift {
            CombineOp::Union
        } else if mods.alt {
            CombineOp::Subtract
        } else {
            self.select_op
        }
    }

    /// Commit the lasso points as a selection (or deselect if too small).
    fn finish_polygon(&mut self, ctx: &egui::Context) {
        let pts = std::mem::take(&mut self.lasso);
        if pts.len() < 3 {
            self.run(&SetSelection { selection: None });
            return;
        }
        let op = self.selection_op(ctx);
        let shape = Selection::polygon(&pts);
        if shape.is_empty() {
            self.run(&SetSelection { selection: None });
        } else {
            self.run(&ModifySelection { shape, op });
        }
    }

    /// The command a brush stroke becomes: paint pixels, paint the mask, or clone.
    /// Flip quick-mask mode; the overlay swaps between red coverage and
    /// marching ants on the next refresh.
    pub(crate) fn toggle_quick_mask(&mut self) {
        self.quick_mask = !self.quick_mask;
        self.status = if self.quick_mask {
            "Quick mask: paint white to select, black to deselect (Q exits)".into()
        } else {
            "Quick mask off".into()
        };
        self.mark(None);
    }

    fn stroke_command(&self, layer: LayerId, points: Vec<StrokePoint>) -> Box<dyn Command> {
        let mut brush = self.make_brush();
        if self.quick_mask && self.tool != Tool::Clone {
            return Box::new(PaintSelection { brush, points });
        }
        if self.tool == Tool::Heal {
            brush.mode = BrushMode::Paint;
            let texture = !self.heal_spot && self.clone_source.is_some();
            let sample = if self.sample_merged {
                SampleSource::Merged
            } else {
                SampleSource::Layer(layer)
            };
            return Box::new(HealStroke {
                layer,
                brush,
                points,
                offset: if texture { self.clone_offset } else { (0, 0) },
                sample,
                texture,
            });
        }
        if self.tool == Tool::Clone {
            brush.mode = BrushMode::Paint;
            let sample = if self.sample_merged {
                SampleSource::Merged
            } else {
                SampleSource::Layer(layer)
            };
            return Box::new(CloneStroke {
                layer,
                brush,
                points,
                offset: self.clone_offset,
                sample,
            });
        }
        if self.editing_mask {
            Box::new(PaintMask { layer, brush, points })
        } else {
            Box::new(PaintStroke { layer, brush, points })
        }
    }

    // ---- shortcuts --------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Modifiers as M;
        // The palette toggle works even while a text field has focus (but
        // not under a modal dialog, which would cover it). On the welcome
        // screen it lists the actions with document-only ones greyed out.
        if self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(M::COMMAND, Key::K)) {
            self.toggle_palette();
        }
        if self.palette.is_some() || ctx.wants_keyboard_input() {
            return;
        }
        // A modal dialog owns the keyboard even when no text field has
        // focus, and a shortcut firing mid-drag would edit the document
        // under an in-progress stroke or move.
        if self.dialog.is_some() || self.drag.is_some() {
            return;
        }
        if self.xform.is_some() {
            let (enter, esc) = ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            if enter {
                self.commit_free_transform();
            }
            if esc {
                self.cancel_free_transform();
            }
            // Everything else (undo, delete, tool switches, grouping) would
            // act on the document underneath the live transform preview.
            return;
        }
        // The Crop tool owns Enter (crop) and Esc (reset) while it is up.
        self.crop_keys(ctx);
        // Esc first cancels an armed "New text" (the next canvas click would
        // otherwise still start a new layer).
        if self.text_new_armed && ctx.input_mut(|i| i.consume_key(M::NONE, Key::Escape)) {
            self.text_new_armed = false;
            self.status = "New text cancelled".into();
        }
        // Esc is "get me out": drop the selection (the polygonal lasso and
        // free transform consume it first for their own cancel).
        if self.editor.doc().selection.is_some()
            && !color_picker::is_open(ctx)
            && !self.gradient.open
            && ctx.input_mut(|i| i.consume_key(M::NONE, Key::Escape))
        {
            self.run(&SetSelection { selection: None });
        }
        // Rebindable command chords (see session::SHORTCUTS for the
        // defaults and Preferences for rebinding); Ctrl+Y stays a fixed
        // redo alias.
        let mut fired: Vec<&'static str> = Vec::new();
        ctx.input_mut(|i| {
            // Stamp visible (Shift+Alt+Cmd+E) holds the merge chords, so
            // it goes first.
            if i.consume_key(M::COMMAND | M::SHIFT | M::ALT, Key::E) {
                fired.push("stamp-visible");
            }
            for (id, ..) in session::SHORTCUTS {
                if let Some((m, k)) = session::resolve_chord(&self.prefs, id) {
                    if i.consume_key(m, k) {
                        fired.push(id);
                    }
                }
            }
            if i.consume_key(M::COMMAND, Key::Y) {
                fired.push("redo");
            }
        });
        // Keys run the same actions as the menus, so a key that can't apply
        // says why in the status bar instead of silently doing nothing (on
        // the welcome screen that leaves only Open).
        for id in fired {
            // Cmd+J is "layer via copy" with a selection and "duplicate
            // layer" without one, as in Photoshop.
            let id = if id == "layer-via-copy" {
                self.cmd_j_action()
            } else {
                id
            };
            self.run_menu_action(id);
        }
        // Canvas zoom: Cmd+= / Cmd+−, and Cmd+0 / Cmd+1 alongside the plain
        // 0 / 1 keys (what Photoshop hands expect). `|` consumes both
        // spellings of zoom-in.
        let (zoom_in, zoom_out, cmd_fit, cmd_actual) = ctx.input_mut(|i| {
            (
                i.consume_key(M::COMMAND, Key::Equals) | i.consume_key(M::COMMAND, Key::Plus),
                i.consume_key(M::COMMAND, Key::Minus),
                i.consume_key(M::COMMAND, Key::Num0),
                i.consume_key(M::COMMAND, Key::Num1),
            )
        });
        for (hit, id) in [
            (zoom_in, "zoom-in"),
            (zoom_out, "zoom-out"),
            (cmd_fit, "fit"),
            (cmd_actual, "actual"),
        ] {
            if hit {
                self.run_menu_action(id);
            }
        }
        let (fill, delete, fill_dialog) = ctx.input(|i| {
            let back = i.key_pressed(Key::Backspace);
            (
                i.modifiers.shift && i.key_pressed(Key::F5),
                i.key_pressed(Key::Delete) || (back && !i.modifiers.shift),
                back && i.modifiers.shift,
            )
        });
        if fill {
            self.run_menu_action("fill");
        }
        // Shift+Backspace: Photoshop's Fill dialog (content-aware or colour).
        if fill_dialog && delete_key_action(self.tool).is_some() {
            self.run_menu_action("fill-dialog");
        }
        if delete {
            if let Some(id) = delete_key_action(self.tool) {
                self.run_menu_action(id);
            }
        }
        let (tool, bigger, smaller, fit, actual, swap_colors, default_colors, quick_mask) = ctx.input(|i| {
            if i.modifiers.command || i.modifiers.alt {
                return (None, false, false, false, false, false, false, false);
            }
            let tool = if i.key_pressed(Key::V) {
                Some(Tool::Move)
            } else if i.key_pressed(Key::B) {
                Some(Tool::Brush)
            } else if i.key_pressed(Key::E) {
                Some(Tool::Eraser)
            } else if i.key_pressed(Key::G) {
                Some(if i.modifiers.shift {
                    Tool::Gradient
                } else {
                    Tool::Bucket
                })
            } else if i.key_pressed(Key::L) {
                Some(if i.modifiers.shift {
                    Tool::PolyLasso
                } else {
                    Tool::Lasso
                })
            } else if i.key_pressed(Key::W) {
                Some(Tool::Wand)
            } else if i.key_pressed(Key::T) {
                Some(Tool::Text)
            } else if i.key_pressed(Key::S) {
                Some(Tool::Clone)
            } else if i.key_pressed(Key::I) {
                Some(Tool::Eyedropper)
            } else if i.key_pressed(Key::M) {
                Some(if i.modifiers.shift {
                    Tool::EllipseSelect
                } else {
                    Tool::RectSelect
                })
            } else if i.key_pressed(Key::J) {
                Some(Tool::Heal)
            } else if i.key_pressed(Key::P) {
                Some(Tool::Pen)
            } else if i.key_pressed(Key::H) {
                Some(Tool::Hand)
            } else if i.key_pressed(Key::C) {
                Some(Tool::Crop)
            } else if i.key_pressed(Key::U) {
                Some(Tool::Shape)
            } else {
                None
            };
            (
                tool,
                i.key_pressed(Key::CloseBracket),
                i.key_pressed(Key::OpenBracket),
                i.key_pressed(Key::Num0),
                i.key_pressed(Key::Num1),
                i.key_pressed(Key::X),
                i.key_pressed(Key::D),
                i.key_pressed(Key::Q),
            )
        });
        if quick_mask {
            self.toggle_quick_mask();
        }
        if swap_colors {
            std::mem::swap(&mut self.brush_rgb, &mut self.bg_rgb);
        }
        if default_colors {
            self.brush_rgb = [0.0; 3];
            self.bg_rgb = [1.0; 3];
        }
        if let Some(t) = tool {
            // Shift+W switches between the Magic Wand and Quick Selection.
            if t == Tool::Wand && ctx.input(|i| i.modifiers.shift) {
                self.quick.on = !self.quick.on;
            }
            self.tool = t;
        }
        let quick = self.tool == Tool::Wand && self.quick.on;
        if bigger {
            if quick {
                self.quick.radius = (self.quick.radius * 1.25).min(300.0);
            } else {
                self.brush.radius = (self.brush.radius * 1.25).min(200.0);
            }
        }
        if smaller {
            if quick {
                self.quick.radius = (self.quick.radius / 1.25).max(1.0);
            } else {
                self.brush.radius = (self.brush.radius / 1.25).max(1.0);
            }
        }
        if fit {
            self.view_cmd = Some(ViewCmd::Fit);
        }
        if actual {
            self.view_cmd = Some(ViewCmd::Actual);
        }
    }

    // ---- right panel ---------------------------------------------------------------

    /// The right dock: Properties, the quick-add chips, Layers. Properties
    /// gets whatever height Layers can spare (Layers always keeps room for
    /// its footer and at least three rows), so on a tall window nothing
    /// scrolls and on a short one both sections stay usable.
    fn side_panel(&mut self, ctx: &egui::Context) {
        // Never let the dock squeeze the canvas below about 60% of the
        // window on narrow screens.
        let max_w = (ctx.screen_rect().width() * 0.4).max(300.0);
        egui::SidePanel::right("side")
            .default_width(320.0)
            .min_width(280.0)
            .max_width(max_w)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(12.0, 10.0)),
            )
            .show(ctx, |ui| {
                raise_controls(ui);
                let rows = self.layer_rows().len().max(1) as f32;
                let quick_id = egui::Id::new("dock-quick-add-h");
                let quick_h = ctx.data(|d| d.get_temp::<f32>(quick_id)).unwrap_or(96.0);
                // Layers get their full height up to ~45% of the dock (never
                // fewer than three rows); Properties gets the rest.
                let avail = ui.available_height();
                let layers_full = layers::HEADER_H + rows * layers::ROW_PITCH + layers::FOOTER_H;
                let layers_min = layers::HEADER_H + 3.0 * layers::ROW_PITCH + layers::FOOTER_H;
                let layers_auto = layers_full.min(layers_min.max(avail * 0.45));
                // The divider below Properties can be dragged; double-click
                // returns to the automatic split.
                let split_id = egui::Id::new("dock-layers-h");
                let layers_max = (avail - quick_h - 120.0).max(layers_min);
                let layers_want = ctx
                    .data_mut(|d| d.get_persisted::<f32>(split_id))
                    .map_or(layers_auto, |h| h.clamp(layers_min, layers_max));
                let props_max = (avail - quick_h - layers_want - 24.0).max(72.0);
                let scroll_out = egui::ScrollArea::vertical()
                    .id_salt("props")
                    .max_height(props_max)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        // Keep the floating scroll bar off the values.
                        egui::Frame::none()
                            .inner_margin(egui::Margin {
                                right: 8.0,
                                ..Default::default()
                            })
                            .show(ui, |ui| self.properties_ui(ui));
                    });
                a11y_scroll(ui.ctx(), &scroll_out, "Properties");
                let (bar, grip) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), Sense::click_and_drag());
                let live = grip.hovered() || grip.dragged();
                if live {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                ui.painter().hline(
                    bar.x_range(),
                    bar.center().y,
                    Stroke::new(1.0, if live { ACCENT } else { LINE }),
                );
                if grip.dragged() {
                    let h = (layers_want - grip.drag_delta().y).clamp(layers_min, layers_max);
                    ctx.data_mut(|d| d.insert_persisted(split_id, h));
                }
                if grip.double_clicked() {
                    ctx.data_mut(|d| d.remove::<f32>(split_id));
                }
                grip.on_hover_text("Drag to give Properties or Layers more room · double-click to reset")
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Other,
                            true,
                            "Resize Properties and Layers",
                        )
                    });
                let top = ui.cursor().top();
                self.quick_add_ui(ui);
                let h = ui.cursor().top() - top;
                ctx.data_mut(|d| d.insert_temp(quick_id, h));
                ui.separator();
                self.layers_ui(ui);
            });
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.debug_popups_input(ctx, raw);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.shot.is_some() {
            return; // a screenshot run never counts as a user exit
        }
        // An intentional exit needs no crash recovery; a stale backup would
        // only raise a misleading prompt next launch.
        session::remove_autosave();
    }
}

impl App {
    /// One UI frame; `update` without the eframe window, so tests can run it.
    fn frame(&mut self, ctx: &egui::Context) {
        if self.liquify.is_some() {
            self.liquify_ui(ctx);
            self.debug_screenshot(ctx);
            return;
        }
        if self.camera_raw.is_some() {
            self.camera_raw_ui(ctx);
            self.debug_screenshot(ctx);
            return;
        }
        if self.export_as.is_some() {
            self.export_as_ui(ctx);
            self.debug_screenshot(ctx);
            return;
        }
        // Files opened from Finder (or the Dock) while running or at launch.
        for path in macos_open::take_pending() {
            self.open_path(&path);
        }
        if !self.no_doc {
            self.clipboard_keys(ctx);
        }
        self.handle_file_drop(ctx);
        // Intercept closing the window while there are unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.any_unsaved() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::ConfirmClose);
        }
        self.shortcuts(ctx);
        self.menu_bar(ctx);
        if self.no_doc {
            self.welcome(ctx);
        } else {
            self.options_bar(ctx);
            self.status_bar(ctx);
            self.history_strip(ctx);
            self.tool_palette(ctx);
            self.side_panel(ctx);
            self.canvas(ctx);
        }
        self.dialogs(ctx);
        self.palette_ui(ctx);
        let unsaved = self.editor.history().len() != self.saved_rev;
        if unsaved && self.drag.is_none() && self.last_autosave.elapsed() > self.prefs.autosave_every() {
            self.last_autosave = std::time::Instant::now();
            session::autosave(self.editor.doc().clone(), self.path.clone());
            self.status = "Autosaved a backup".into();
        }
        if self.dirty && !self.no_doc {
            self.refresh(ctx);
            ctx.request_repaint();
        }
        let name = self
            .path
            .as_ref()
            .map(|p| file_name(&p.to_string_lossy()))
            .unwrap_or_else(|| self.untitled.clone());
        let unsaved = if self.editor.history().len() != self.saved_rev {
            " •"
        } else {
            ""
        };
        let title = if self.no_doc {
            "Lumenply".to_string()
        } else {
            format!("{name}{unsaved} — Lumenply")
        };
        if self.last_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        self.splash_ui(ctx);
        self.debug_screenshot(ctx);
    }
}

// ---- free helpers --------------------------------------------------------------------

fn blank(w: u32, h: u32) -> Editor {
    let mut ed = Editor::new(Document::new(w, h));
    let paper = Raster::filled(w, h, lumenply_tiles::Rgba::WHITE);
    let _ = ed.execute(&AddPixelLayer::from_raster("Background", paper, 0, 0));
    ed
}

fn srgb_to_linear_f(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// An sRGB swatch (0..1 per channel) as straight linear RGBA.
fn linear_rgba(rgb: [f32; 3], alpha: f32) -> [f32; 4] {
    [
        srgb_to_linear_f(rgb[0]),
        srgb_to_linear_f(rgb[1]),
        srgb_to_linear_f(rgb[2]),
        alpha,
    ]
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or("Image".to_string(), |n| n.to_string_lossy().into_owned())
}

fn adjustment_presets() -> Vec<(&'static str, Adjustment)> {
    vec![
        (
            "Brightness/Contrast",
            Adjustment::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
        ),
        ("Levels", Adjustment::levels_default()),
        (
            "Curves",
            Adjustment::Curves {
                points: vec![[0.0, 0.0], [1.0, 1.0]],
            },
        ),
        (
            "Exposure",
            Adjustment::Exposure {
                exposure: 0.0,
                offset: 0.0,
                gamma: 1.0,
            },
        ),
        (
            "Vibrance",
            Adjustment::Vibrance {
                vibrance: 0.0,
                saturation: 0.0,
            },
        ),
        (
            "Hue/Saturation",
            Adjustment::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
        ),
        ("Color Balance", Adjustment::color_balance_default()),
        ("Black & White", Adjustment::black_white_default()),
        ("Threshold", Adjustment::Threshold { level: 0.5 }),
        ("Posterize", Adjustment::Posterize { levels: 4 }),
        ("Invert", Adjustment::Invert),
        ("Gradient Map", Adjustment::gradient_map_default()),
        ("Channel Mixer", Adjustment::channel_mixer_default()),
        ("Photo Filter", Adjustment::photo_filter_default()),
        ("Selective Color", Adjustment::selective_color_default()),
    ]
}

fn filter_presets() -> Vec<(&'static str, Filter)> {
    vec![
        ("Gaussian Blur", Filter::GaussianBlur { radius: 8.0 }),
        ("Box Blur", Filter::BoxBlur { radius: 5.0 }),
        (
            "Sharpen",
            Filter::Sharpen {
                amount: 1.0,
                radius: 2.0,
            },
        ),
        ("Add Noise", Filter::Noise { amount: 0.1 }),
        (
            "Motion Blur",
            Filter::MotionBlur {
                angle: 0.0,
                distance: 20.0,
            },
        ),
        ("Median", Filter::Median { radius: 2.0 }),
        ("High Pass", Filter::HighPass { radius: 4.0 }),
        ("Mosaic", Filter::Mosaic { size: 16.0 }),
        (
            "Emboss",
            Filter::Emboss {
                angle: 135.0,
                height: 3.0,
                amount: 1.0,
            },
        ),
        ("Find Edges", Filter::FindEdges),
        (
            "Surface Blur",
            Filter::SurfaceBlur {
                radius: 5.0,
                threshold: 15.0,
            },
        ),
        (
            "Lens Blur",
            Filter::LensBlur {
                radius: 10.0,
                highlights: 0.3,
            },
        ),
        (
            "Dust & Scratches",
            Filter::DustScratches {
                radius: 2.0,
                threshold: 10.0,
            },
        ),
    ]
}

/// The Filter menu's Photoshop-style submenu for a filter.
fn filter_category(f: &Filter) -> &'static str {
    match f {
        Filter::GaussianBlur { .. }
        | Filter::BoxBlur { .. }
        | Filter::MotionBlur { .. }
        | Filter::SurfaceBlur { .. }
        | Filter::LensBlur { .. } => "Blur",
        Filter::Noise { .. } | Filter::Median { .. } | Filter::DustScratches { .. } => "Noise",
        Filter::Mosaic { .. } => "Pixelate",
        Filter::Sharpen { .. } => "Sharpen",
        Filter::Emboss { .. } | Filter::FindEdges => "Stylize",
        Filter::HighPass { .. } => "Other",
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn delete_key_never_clears_pixels_while_the_pen_owns_it() {
        // The pen deletes the selected path node with Delete/Backspace;
        // clearing the layer as well would erase pixels the user never meant
        // to touch.
        assert_eq!(delete_key_action(Tool::Pen), None);
        assert_eq!(delete_key_action(Tool::Brush), Some("clear"));
        assert_eq!(delete_key_action(Tool::Move), Some("clear"));
        assert_eq!(delete_key_action(Tool::RectSelect), Some("clear"));
        assert_eq!(
            delete_key_action(Tool::Crop),
            None,
            "Delete never clears under a crop frame"
        );
    }
}

#[cfg(test)]
mod smart_tests {
    use super::*;

    #[test]
    fn smart_objects_flip_losslessly_from_every_flip_entry_point() {
        let mut app = App::launch(&["--demo".to_string()]);
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app.run_menu_action("smart-object");
        assert!(app.active_layer().unwrap().smart_layer().is_some());

        // Flips used to error (FlipLayer is pixel-only); they now compose
        // into the smart transform, and the registry no longer blocks them.
        assert_eq!(app.action_block("flip-h"), None);
        app.run_menu_action("flip-h");
        let t = app.active_layer().unwrap().smart_layer().unwrap().transform;
        assert!((t.a + 1.0).abs() < 1e-6, "mirrored on x: {t:?}");

        // A second flip returns exactly to the original pixels' mapping.
        app.run_menu_action("flip-h");
        let s = app.active_layer().unwrap().smart_layer().unwrap();
        assert!(
            (s.transform.a - 1.0).abs() < 1e-6 && s.transform.tx.abs() < 1e-3,
            "{:?}",
            s.transform
        );
        assert!(
            app.active_layer().unwrap().smart_layer().is_some(),
            "still a smart object"
        );
        assert_eq!(app.editor.history().len(), 3, "convert + two flips");
    }
    #[test]
    fn untitled_numbers_never_skip_after_imports() {
        let mut app = App::launch(&["--demo".to_string()]);
        let blank = || Editor::new(Document::new(8, 8));
        app.open_in_new_tab(blank(), None);
        assert_eq!(app.untitled, "Untitled-1", "the demo does not use up a number");
        // An import relabels its tab with the file name.
        app.open_in_new_tab(blank(), None);
        app.untitled = "photo.jpg".into();
        app.open_in_new_tab(blank(), None);
        assert_eq!(app.untitled, "Untitled-2", "the import did not skip a number");
    }
    #[test]
    fn escape_cancels_an_armed_new_text() {
        let mut app = App::launch(&["--demo".to_string()]);
        app.text_new_armed = true;
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run(raw, |ctx| app.shortcuts(ctx));
        assert!(!app.text_new_armed, "Esc disarms New text");
        assert_eq!(app.status, "New text cancelled");
    }
}

#[cfg(test)]
pub(crate) mod a11y_tests {
    use super::*;
    use egui::accesskit::{Action, Role};

    /// Runs frames and returns every keyboard-reachable accessibility node
    /// that a screen reader could only announce as an unnamed control.
    /// egui's own scroll bars (thin unnamed strips) can't be named from
    /// outside egui and are left out.
    pub(crate) fn nameless(app: &mut App, ctx: &egui::Context) -> Vec<String> {
        let mut found = Vec::new();
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(raw, |ctx| app.frame(ctx));
            let update = out
                .platform_output
                .accesskit_update
                .expect("accesskit is enabled");
            found = update
                .nodes
                .iter()
                .filter(|(_, n)| n.supports_action(Action::Focus))
                .filter(|(_, n)| n.name().is_none_or(|s| s.trim().is_empty()))
                .filter(|(_, n)| {
                    let b = n.bounds().unwrap_or_default();
                    !(n.role() == Role::Unknown && b.width().min(b.height()) <= 10.0)
                })
                .map(|(_, n)| {
                    let b = n.bounds().unwrap_or_default();
                    format!(
                        "{:?} at ({:.0}, {:.0}) {:.0}×{:.0}",
                        n.role(),
                        b.x0,
                        b.y0,
                        b.width(),
                        b.height()
                    )
                })
                .collect();
        }
        found
    }

    /// The app with no dialog up, and no autosave while frames run: a
    /// backup left in the test data folder would greet every later launch
    /// with the Recover dialog.
    pub(crate) fn launch(args: &[String]) -> App {
        let mut app = App::launch(args);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app
    }

    pub(crate) fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.enable_accesskit();
        ctx
    }

    #[test]
    fn the_welcome_screen_names_every_control() {
        let mut app = launch(&[]);
        assert!(app.no_doc);
        assert_eq!(nameless(&mut app, &ctx()), Vec::<String>::new());
    }

    #[test]
    fn the_editor_names_every_control_for_every_tool() {
        let mut app = launch(&["--demo".to_string()]);
        let ctx = ctx();
        // Rulers (drag-out targets) and the grid on, so they are covered.
        app.prefs.show_rulers = true;
        app.prefs.show_grid = true;
        for tool in Tool::ALL {
            app.tool = tool;
            let missing = nameless(&mut app, &ctx);
            assert_eq!(missing, Vec::<String>::new(), "with the {} tool", tool.name());
        }
    }

    #[test]
    fn the_colour_picker_names_every_control() {
        let mut app = launch(&["--demo".to_string()]);
        let ctx = ctx();
        ctx.memory_mut(|m| m.open_popup(color_picker::fg_picker()));
        let missing = nameless(&mut app, &ctx);
        assert!(
            ctx.memory(|m| m.is_popup_open(color_picker::fg_picker())),
            "the picker stayed open"
        );
        assert_eq!(missing, Vec::<String>::new());
    }

    #[test]
    fn every_dialog_layer_kind_and_effect_names_every_control() {
        // A tiny document: every frame re-renders it in an unoptimised build.
        let mut app = launch(&[]);
        app.open_in_new_tab(blank(64, 64), None);
        let ctx = ctx();
        let mut failures = Vec::new();
        let mut check = |app: &mut App, what: &str| {
            let missing = nameless(app, &ctx);
            if !missing.is_empty() {
                failures.push(format!("{what}: {missing:?}"));
            }
        };
        let top = |app: &App| app.editor.doc().layers().last().unwrap().id;
        let bg = top(&app);
        app.set_active(Some(bg));
        check(&mut app, "pixel layer");
        app.run(&SetLayerEffects {
            layer: bg,
            effects: lumenply_doc::LayerEffects {
                drop_shadow: Some(Default::default()),
                outer_glow: Some(Default::default()),
                color_overlay: Some(Default::default()),
                gradient_overlay: Some(Default::default()),
                inner_shadow: Some(Default::default()),
                inner_glow: Some(Default::default()),
                bevel: Some(Default::default()),
                stroke: Some(Default::default()),
            },
        });
        check(&mut app, "every layer effect on");
        app.run_menu_action("add-mask");
        assert!(app.editing_mask);
        check(&mut app, "editing a mask");
        app.run(&AddTextLayer {
            text: TextLayer::new("Hi", 8.0, 8.0, 12.0, [0.0, 0.0, 0.0, 1.0]),
            above: None,
        });
        let text = top(&app);
        app.set_active(Some(text));
        app.tool = Tool::Text;
        check(&mut app, "text layer with the Text tool");
        app.tool = Tool::Brush;
        app.set_active(Some(bg));
        app.run_menu_action("smart-object");
        assert!(app.active_layer().unwrap().smart_layer().is_some());
        check(&mut app, "smart object");
        app.run_menu_action("group");
        assert!(matches!(
            app.active_layer().unwrap().content,
            LayerContent::Group(_)
        ));
        check(&mut app, "group");
        // One at a time: stacked live filters multiply the render cost.
        for (name, adj) in adjustment_presets() {
            app.add_adjustment(adj);
            check(&mut app, &format!("{name} adjustment layer"));
            app.run_menu_action("undo");
        }
        for (name, id) in [("solid", "fill-solid"), ("gradient", "fill-gradient")] {
            app.run_menu_action(id);
            assert!(app.active_layer().unwrap().fill_layer().is_some());
            check(&mut app, &format!("{name} fill layer"));
            app.run_menu_action("undo");
        }
        for (kind, extra) in [
            ("rectangle", "shape:fill=gradient"),
            ("polygon", "shape:stroke=2"),
            ("line", "shape:arrows=end"),
            ("heart", "shape:fill=none"),
        ] {
            app.debug_shape(&ctx, &format!("shape:kind={kind}"));
            app.debug_shape(&ctx, extra);
            app.debug_shape(&ctx, "shape:draw=4:4:40:30");
            assert!(app.active_layer().unwrap().shape_layer().is_some());
            check(&mut app, &format!("{kind} shape layer"));
            app.run_menu_action("undo");
        }
        for (name, f) in filter_presets() {
            app.add_filter_layer(f.clone());
            check(&mut app, &format!("{name} filter layer"));
            app.run_menu_action("undo");
            app.dialog = Some(Dialog::Filter(f));
            check(&mut app, &format!("{name} filter dialog"));
        }
        let dialogs = [
            ("New", Dialog::New(1920, 1080)),
            ("Image size", Dialog::ImageSize(800, 600, true)),
            ("Canvas size", Dialog::CanvasSize(800, 600, (0.5, 0.5))),
            ("Export JPEG", Dialog::ExportJpeg("out.jpg".into(), 90)),
            ("Confirm close", Dialog::ConfirmClose),
            ("Confirm close tab", Dialog::ConfirmCloseTab(0)),
            ("Recover", Dialog::Recover),
            ("Preferences", Dialog::Preferences(app.prefs.clone(), None)),
            ("Colour range", Dialog::ColorRange(25.0, false)),
            ("New guide", Dialog::NewGuide(true, 32.0)),
            ("About", Dialog::About),
            ("Save selection", Dialog::SaveSelection("Sky".into())),
            ("Trim", Dialog::Trim(true)),
            ("Keyboard shortcuts", Dialog::Shortcuts),
            ("Rotate canvas", Dialog::RotateBy(15.0, true)),
            (
                "Load selection",
                Dialog::LoadSelection(0, CombineOp::Replace, false),
            ),
            ("Expand selection", Dialog::SelectEdge(EdgeOp::Expand(4.0), false)),
            ("Border selection", Dialog::SelectEdge(EdgeOp::Border(8.0), false)),
            ("Content-aware fill", Dialog::Fill(true, 64.0, 0)),
            ("Fill with colour", Dialog::Fill(false, 64.0, 0)),
        ];
        for (name, d) in dialogs {
            app.dialog = Some(d);
            check(&mut app, &format!("{name} dialog"));
        }
        app.dialog = None;
        app.toggle_palette();
        check(&mut app, "command palette");
        app.toggle_palette();
        app.prefs.history_collapsed = !app.prefs.history_collapsed;
        check(&mut app, "history strip toggled");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn the_roles_a_screen_reader_announces_are_the_right_ones() {
        let mut app = launch(&["--demo".to_string()]);
        let ctx = ctx();
        ctx.memory_mut(|m| m.open_popup(color_picker::fg_picker()));
        let mut roles = std::collections::HashMap::new();
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(raw, |ctx| app.frame(ctx));
            roles = out
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .filter_map(|(_, n)| Some((n.name()?.to_string(), n.role())))
                .collect();
        }
        assert_eq!(roles.get("Swap colours"), Some(&Role::Button));
        assert_eq!(roles.get("Hue"), Some(&Role::Slider));
        assert_eq!(roles.get("Pick colour from canvas"), Some(&Role::Button));
        let fg = roles
            .keys()
            .find(|k| k.starts_with("Foreground colour #"))
            .expect("fg well named");
        assert_eq!(roles[fg], Role::ColorWell);
    }
}
