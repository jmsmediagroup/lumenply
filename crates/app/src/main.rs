//! Desktop editor shell built on egui.
//!
//! A thin layer over `nge_core::Editor`: every edit is a `Command`, so undo,
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
use nge_core::commands::*;
use nge_core::{Command, Editor};
use nge_doc::{
    Adjustment, BlendMode, CombineOp, Document, Filter, Layer, LayerContent, LayerId, Selection, TextLayer,
};
use nge_io::project;
use nge_tiles::{Affine, Raster, Rect};

const ACCENT: Color32 = Color32::from_rgb(58, 112, 196);
const THUMB: (usize, usize) = (44, 30);

fn main() -> Result<(), eframe::Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("NGE"),
        ..Default::default()
    };
    eframe::run_native(
        "NGE",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, &args)))),
    )
}

fn is_image_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e))
}

fn is_psd_path(p: &str) -> bool {
    p.to_ascii_lowercase().ends_with(".psd")
}

// ---- small enums ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Move,
    RectSelect,
    EllipseSelect,
    Lasso,
    PolyLasso,
    Wand,
    Brush,
    Eraser,
    Clone,
    Bucket,
    Gradient,
    Text,
    Eyedropper,
    Hand,
}

impl Tool {
    const ALL: [Tool; 14] = [
        Tool::Move,
        Tool::RectSelect,
        Tool::EllipseSelect,
        Tool::Lasso,
        Tool::PolyLasso,
        Tool::Wand,
        Tool::Brush,
        Tool::Eraser,
        Tool::Clone,
        Tool::Bucket,
        Tool::Gradient,
        Tool::Text,
        Tool::Eyedropper,
        Tool::Hand,
    ];

    fn name(self) -> &'static str {
        match self {
            Tool::Move => "Move",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Clone => "Clone Stamp",
            Tool::Bucket => "Paint Bucket",
            Tool::Gradient => "Gradient",
            Tool::Text => "Text",
            Tool::Eyedropper => "Eyedropper",
            Tool::RectSelect => "Rectangular Marquee",
            Tool::EllipseSelect => "Elliptical Marquee",
            Tool::Lasso => "Lasso",
            Tool::PolyLasso => "Polygonal Lasso",
            Tool::Wand => "Magic Wand",
            Tool::Hand => "Hand",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Tool::Move => "Move (V)",
            Tool::Brush => "Brush (B)",
            Tool::Eraser => "Eraser (E)",
            Tool::Clone => "Clone Stamp (S) — Alt+click or 'Pick source' to set the source",
            Tool::Bucket => "Paint Bucket (G)",
            Tool::Gradient => "Gradient (Shift+G)",
            Tool::Text => "Text (T)",
            Tool::Eyedropper => "Eyedropper (I)",
            Tool::RectSelect => "Rectangular Marquee (M)",
            Tool::EllipseSelect => "Elliptical Marquee (Shift+M)",
            Tool::Lasso => "Lasso (L)",
            Tool::PolyLasso => "Polygonal Lasso (Shift+L)",
            Tool::Wand => "Magic Wand (W)",
            Tool::Hand => "Hand (H)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pixel,
    Group,
    Adjustment,
    Filter,
    Text,
}

struct LayerRow {
    id: LayerId,
    name: String,
    visible: bool,
    opacity: f32,
    kind: Kind,
    masked: bool,
    mask_enabled: bool,
    depth: usize,
    collapsed: bool,
}

enum Dialog {
    Open(String),
    OpenImage(String),
    PlaceImage(String),
    Save(String),
    Export(String),
    ExportJpeg(String, u8),
    ExportPsd(String),
    New(u32, u32),
    Filter(Filter),
    CanvasSize(u32, u32, (f32, f32)),
    ImageSize(u32, u32, bool),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewCmd {
    Fit,
    Actual,
}

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    Corner(usize),
    Inside,
    Rotate,
}

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Stroke,
    Select,
    Lasso,
    Move,
    Gradient,
    Xform(Handle),
}

/// In-progress free transform of one layer.
#[derive(Clone)]
struct Xform {
    layer: LayerId,
    bounds: Rect,
    scale: f32,
    angle: f32,
    dx: f32,
    dy: f32,
    // values at the start of the current drag
    base: (f32, f32, f32, f32),
    last_preview: Rect,
}

impl Xform {
    fn center(&self) -> (f32, f32) {
        (
            self.bounds.x as f32 + self.bounds.w as f32 / 2.0,
            self.bounds.y as f32 + self.bounds.h as f32 / 2.0,
        )
    }

    fn affine(&self) -> Affine {
        let (cx, cy) = self.center();
        Affine::around(cx, cy, self.scale, self.scale, self.angle).then(&Affine::translate(self.dx, self.dy))
    }

    /// Transformed corners, in document space.
    fn corners(&self) -> [(f32, f32); 4] {
        let a = self.affine();
        let b = self.bounds;
        [
            a.apply(b.x as f32, b.y as f32),
            a.apply(b.right() as f32, b.y as f32),
            a.apply(b.right() as f32, b.bottom() as f32),
            a.apply(b.x as f32, b.bottom() as f32),
        ]
    }

    fn bbox(&self) -> Rect {
        self.affine().transform_rect(self.bounds)
    }
}

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
    gradient_kind: GradientKind,
    gradient_to_transparent: bool,
    text_size: f32,
    text_bold: bool,
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

    dialog: Option<Dialog>,
    filter_previewed: bool,
    status: String,
}

impl App {
    /// `nge-app [--demo | file.nge | image.png] [--place image.png]...`
    fn new(cc: &eframe::CreationContext<'_>, args: &[String]) -> Self {
        let mut v = egui::Visuals::dark();
        v.panel_fill = Color32::from_gray(33);
        v.window_fill = Color32::from_gray(38);
        v.selection.bg_fill = ACCENT;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(52));
        cc.egui_ctx.set_visuals(v);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        cc.egui_ctx.set_style(style);

        let mut status = String::from("Ready");
        let first = args.first().filter(|a| *a != "--place").map(String::as_str);
        let (editor, path) = match first {
            Some("--demo") => (nge_core::demo::build(1200, 800).expect("demo document"), None),
            Some(p) if is_image_path(p) => match nge_io::load(p) {
                Ok(raster) => {
                    let mut ed = Editor::new(Document::new(raster.width, raster.height));
                    let _ = ed.execute(&AddPixelLayer::from_raster("Background", raster, 0, 0));
                    (ed, None)
                }
                Err(e) => {
                    status = format!("Could not open {p}: {e}");
                    (blank(1200, 800), None)
                }
            },
            Some(p) if is_psd_path(p) => match nge_io::psd::load(p) {
                Ok(rep) => {
                    if !rep.warnings.is_empty() {
                        status = format!("Imported with notes: {}", rep.warnings.join("; "));
                    }
                    (Editor::new(rep.value), None)
                }
                Err(e) => {
                    status = format!("Could not import {p}: {e}");
                    (blank(1200, 800), None)
                }
            },
            Some(p) => match project::load(p) {
                Ok(doc) => (Editor::new(doc), Some(PathBuf::from(p))),
                Err(e) => {
                    status = format!("Could not open {p}: {e}");
                    (blank(1200, 800), None)
                }
            },
            None => (blank(1200, 800), None),
        };
        let mut app = App {
            editor,
            tool: Tool::Brush,
            active: None,
            selected: Vec::new(),
            editing_mask: false,
            path,
            brush: Brush {
                radius: 14.0,
                hardness: 0.7,
                color: [0.0, 0.0, 0.0, 1.0],
                spacing: 0.12,
                mode: BrushMode::Paint,
            },
            brush_rgb: [0.10, 0.18, 0.55],
            bg_rgb: [1.0, 1.0, 1.0],
            select_op: CombineOp::Replace,
            feather: 12.0,
            tolerance: 0.12,
            contiguous: true,
            sample_merged: false,
            gradient_kind: GradientKind::Linear,
            gradient_to_transparent: false,
            text_size: 72.0,
            text_bold: false,
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
            lasso: Vec::new(),
            curve_drag: None,
            cursor_doc: None,
            move_offset: (0, 0),
            renaming: None,
            xform: None,
            xform_scale: 100.0,
            xform_angle: 0.0,
            cb_tone: 1,
            dialog: None,
            filter_previewed: false,
            status,
        };
        app.select_top();
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--place" {
                if let Some(p) = args.get(i + 1) {
                    app.place_image(p);
                }
                i += 1;
            }
            i += 1;
        }
        app
    }

    // ---- document plumbing -------------------------------------------------

    fn set_active(&mut self, id: Option<LayerId>) {
        self.active = id;
        self.selected = id.into_iter().collect();
        self.editing_mask = false;
        self.xform = None;
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
        match self.editor.execute(cmd) {
            Ok(()) => {
                let r = self.editor.last_affected();
                self.mark(r);
                self.fix_active();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn run_coalescing(&mut self, cmd: &dyn Command, key: &str) {
        match self.editor.execute_coalescing(cmd, key) {
            Ok(()) => {
                let r = self.editor.last_affected();
                self.mark(r);
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn set_doc(&mut self, editor: Editor, path: Option<PathBuf>) {
        self.editor = editor;
        self.path = path;
        self.select_top();
        self.mark(None);
        self.view_cmd = Some(ViewCmd::Fit);
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

    /// Shared editor for a text layer's content and style. Returns true
    /// when an edit finished (for undo coalescing).
    fn text_controls(&mut self, ui: &mut egui::Ui, id: LayerId, mut t: TextLayer, multiline: bool) {
        let before = t.clone();
        let mut finished = false;
        let r = if multiline {
            ui.add(
                egui::TextEdit::multiline(&mut t.text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            )
        } else {
            ui.add(egui::TextEdit::singleline(&mut t.text).desired_width(260.0))
        };
        finished |= r.lost_focus();
        let rs = ui.add(
            egui::Slider::new(&mut t.size, 6.0..=400.0)
                .logarithmic(true)
                .suffix(" px")
                .text("Size"),
        );
        finished |= rs.drag_stopped() || (rs.changed() && !rs.dragged());
        if ui.checkbox(&mut t.bold, "Bold").changed() {
            finished = true;
        }
        let mut rgb = [
            nge_io::linear_to_srgb(t.color[0]) as f32 / 255.0,
            nge_io::linear_to_srgb(t.color[1]) as f32 / 255.0,
            nge_io::linear_to_srgb(t.color[2]) as f32 / 255.0,
        ];
        if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
            t.color = linear_rgba(rgb, t.color[3]);
        }
        if t != before {
            self.run_coalescing(&SetText { layer: id, text: t }, &format!("text-{id}"));
        }
        if finished {
            self.editor.end_coalescing();
        }
    }

    fn active_has_mask(&self) -> bool {
        self.active_layer().is_some_and(|l| l.mask.is_some())
    }

    fn make_brush(&self) -> Brush {
        let mut b = self.brush;
        b.color = linear_rgba(self.brush_rgb, self.brush.color[3].max(0.0));
        b.mode = if self.tool == Tool::Eraser {
            BrushMode::Erase
        } else {
            BrushMode::Paint
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

    fn gradient_command(&self, layer: LayerId, start: (f32, f32), end: (f32, f32)) -> GradientFill {
        let from = linear_rgba(self.brush_rgb, 1.0);
        let to = if self.gradient_to_transparent {
            [from[0], from[1], from[2], 0.0]
        } else {
            linear_rgba(self.bg_rgb, 1.0)
        };
        GradientFill {
            layer,
            start,
            end,
            colors: [from, to],
            kind: self.gradient_kind,
        }
    }

    /// The command a brush stroke becomes: paint pixels, paint the mask, or clone.
    fn stroke_command(&self, layer: LayerId, points: Vec<StrokePoint>) -> Box<dyn Command> {
        let mut brush = self.make_brush();
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

    // ---- files -----------------------------------------------------------------

    fn open_path(&mut self, path: &str) {
        if is_psd_path(path) {
            match nge_io::psd::load(path) {
                Ok(rep) => {
                    let n = rep.warnings.len();
                    self.set_doc(Editor::new(rep.value), None);
                    self.status = if n == 0 {
                        format!("Imported {path}")
                    } else {
                        format!("Imported {path} ({n} items skipped: {})", rep.warnings.join("; "))
                    };
                }
                Err(e) => self.status = format!("Could not import {path}: {e}"),
            }
            return;
        }
        if is_image_path(path) {
            self.open_image(path);
            return;
        }
        match project::load(path) {
            Ok(doc) => {
                self.set_doc(Editor::new(doc), Some(PathBuf::from(path)));
                self.status = format!("Opened {path}");
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    fn export_psd(&mut self, path: &str) {
        match nge_io::psd::save(path, self.editor.doc()) {
            Ok(rep) => {
                self.status = if rep.warnings.is_empty() {
                    format!("Exported {path}")
                } else {
                    format!("Exported {path}; not carried over: {}", rep.warnings.join("; "))
                };
            }
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    fn open_image(&mut self, path: &str) {
        match nge_io::load(path) {
            Ok(raster) => {
                let (w, h) = (raster.width, raster.height);
                let mut ed = Editor::new(Document::new(w, h));
                let _ = ed.execute(&AddPixelLayer::from_raster(file_name(path), raster, 0, 0));
                self.set_doc(ed, None);
                self.status = format!("Opened {path} ({w}×{h})");
            }
            Err(e) => self.status = format!("Could not open {path}: {e}"),
        }
    }

    fn place_image(&mut self, path: &str) {
        match nge_io::load(path) {
            Ok(raster) => {
                let doc = self.editor.doc();
                let x = (doc.width as i32 - raster.width as i32) / 2;
                let y = (doc.height as i32 - raster.height as i32) / 2;
                self.run(&AddPixelLayer::from_raster(file_name(path), raster, x, y));
                self.select_top();
                self.status = format!("Placed {path}");
            }
            Err(e) => self.status = format!("Could not place {path}: {e}"),
        }
    }

    fn save_path(&mut self, path: &str) {
        match project::save(path, self.editor.doc()) {
            Ok(()) => {
                self.path = Some(PathBuf::from(path));
                self.status = format!("Saved {path}");
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
    }

    fn export_png(&mut self, path: &str) {
        let flat = nge_render::composite_raster(self.editor.doc());
        match nge_io::save_png(path, &flat) {
            Ok(()) => self.status = format!("Exported {path}"),
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    fn export_jpeg(&mut self, path: &str, quality: u8) {
        let flat = nge_render::composite_raster(self.editor.doc());
        match nge_io::save_jpeg(path, &flat, quality) {
            Ok(()) => self.status = format!("Exported {path} (quality {quality})"),
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }

    // ---- edits used by menus and panels ----------------------------------------------

    fn undo(&mut self) {
        if let Some(l) = self.editor.undo() {
            self.status = format!("Undid {l}");
            self.mark(None);
            self.fix_active();
        }
    }

    fn redo(&mut self) {
        if let Some(l) = self.editor.redo() {
            self.status = format!("Redid {l}");
            self.mark(None);
            self.fix_active();
        }
    }

    fn fill_active(&mut self) {
        if let Some(layer) = self.active {
            let color = self.make_brush().color;
            self.run(&Fill { layer, color });
        }
    }

    fn clear_active(&mut self) {
        if let Some(layer) = self.active {
            self.run(&Clear { layer });
        }
    }

    fn reorder_active(&mut self, delta: i32) {
        if let Some(layer) = self.active {
            self.run(&ReorderLayer { layer, delta });
        }
    }

    fn flip_active(&mut self, horizontal: bool) {
        if let Some(layer) = self.active {
            self.run(&FlipLayer { layer, horizontal });
        }
    }

    fn add_pixel_layer(&mut self) {
        let n = self.editor.doc().layer_count() + 1;
        self.run(&AddPixelLayer::new(format!("Layer {n}")));
        self.select_top();
    }

    fn add_adjustment(&mut self, adj: Adjustment) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddAdjustmentLayer::new(adj);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    fn add_filter_layer(&mut self, f: Filter) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddFilterLayer::new(f);
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    fn delete_active(&mut self) {
        if let Some(id) = self.active {
            self.run(&RemoveLayer { layer: id });
            self.select_top();
        }
    }

    /// Group the multi-selection, or the active layer with the one below it.
    fn group_selected(&mut self) {
        let doc = self.editor.doc();
        let mut ids: Vec<LayerId> = self.selected.clone();
        if ids.len() < 2 {
            let Some(active) = self.active else { return };
            let parent = doc.parent_of(active);
            let siblings: Vec<LayerId> = match parent {
                None => doc.layers().iter().map(|l| l.id).collect(),
                Some(p) => doc.layer(p).map_or(Vec::new(), |g| {
                    g.children().unwrap_or(&[]).iter().map(|l| l.id).collect()
                }),
            };
            let i = siblings.iter().position(|id| *id == active).unwrap_or(0);
            ids = vec![active];
            if i > 0 {
                ids.push(siblings[i - 1]);
            }
        }
        let new_id = doc.next_id();
        let n = doc.layer_count();
        self.run(&GroupLayers {
            layers: ids,
            name: format!("Group {}", n),
        });
        if self.editor.doc().layer(new_id).is_some() {
            self.set_active(Some(new_id));
        }
    }

    fn ungroup_active(&mut self) {
        if let Some(id) = self.active {
            self.run(&UngroupLayer { layer: id });
            self.select_top();
        }
    }

    fn begin_free_transform(&mut self) {
        let Some(id) = self.active else { return };
        let Some(b) = self
            .active_layer()
            .and_then(|l| l.pixels())
            .and_then(|p| p.content_bounds())
        else {
            self.status = "Free transform needs a pixel layer with content".into();
            return;
        };
        self.tool = Tool::Move;
        self.xform = Some(Xform {
            layer: id,
            bounds: b,
            scale: 1.0,
            angle: 0.0,
            dx: 0.0,
            dy: 0.0,
            base: (1.0, 0.0, 0.0, 0.0),
            last_preview: b,
        });
        self.status = "Free transform: drag corners to scale, outside to rotate, inside to move".into();
    }

    fn commit_free_transform(&mut self) {
        if let Some(x) = self.xform.take() {
            let t = x.affine();
            if t.integer_translation() != Some((0, 0)) {
                self.run(&TransformLayer {
                    layer: x.layer,
                    transform: t,
                });
            }
            self.mark(None);
        }
    }

    fn cancel_free_transform(&mut self) {
        if self.xform.take().is_some() {
            self.mark(None);
            self.status = "Transform cancelled".into();
        }
    }

    // ---- rendering ---------------------------------------------------------------------

    fn refresh(&mut self, ctx: &egui::Context) {
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let partial_ok = self
            .last_flat
            .as_ref()
            .is_some_and(|f| f.width == doc.width && f.height == doc.height)
            && self.canvas_tex.is_some();
        match self.dirty_rect.take() {
            Some(r) if partial_ok => {
                let r = r.intersect(&canvas);
                if !r.is_empty() {
                    let patch = nge_render::composite_rect(doc, r).to_raster(r);
                    if let Some(flat) = self.last_flat.as_mut() {
                        for y in 0..r.h {
                            for x in 0..r.w {
                                flat.set(r.x as u32 + x, r.y as u32 + y, patch.get(x, y));
                            }
                        }
                    }
                    if let Some(tex) = self.canvas_tex.as_mut() {
                        tex.set_partial(
                            [r.x as usize, r.y as usize],
                            raster_to_image(&patch),
                            nearest_when_zoomed(),
                        );
                    }
                }
                self.refresh_thumbs(ctx, false);
            }
            _ => {
                let flat = nge_render::composite_raster(doc);
                let img = raster_to_image(&flat);
                self.last_flat = Some(flat);
                upload(&mut self.canvas_tex, ctx, "canvas", img, nearest_when_zoomed());
                match selection_overlay(self.editor.doc()) {
                    Some(img) => upload(
                        &mut self.overlay_tex,
                        ctx,
                        "selection",
                        img,
                        egui::TextureOptions::NEAREST,
                    ),
                    None => self.overlay_tex = None,
                }
                self.refresh_thumbs(ctx, true);
            }
        }
        self.dirty = false;
    }

    fn refresh_thumbs(&mut self, ctx: &egui::Context, groups_too: bool) {
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let mut thumbs: Vec<(LayerId, egui::ColorImage)> = Vec::new();
        let mut masks: Vec<(LayerId, egui::ColorImage)> = Vec::new();
        doc.for_each_layer(|l| {
            match &l.content {
                LayerContent::Pixel(store) => {
                    thumbs.push((l.id, thumb_image(canvas, |x, y| store.get_pixel(x, y))))
                }
                LayerContent::Group(children) if groups_too => {
                    let flat = nge_render::composite_layers(children, canvas, canvas);
                    thumbs.push((l.id, thumb_image(canvas, |x, y| flat.get_pixel(x, y))));
                }
                _ => {}
            }
            if let Some(m) = &l.mask {
                masks.push((
                    l.id,
                    thumb_image(canvas, |x, y| {
                        let v = m.value(x, y);
                        nge_tiles::Rgba::new(v, v, v, 1.0)
                    }),
                ));
            }
        });
        let live: Vec<LayerId> = {
            let mut v = Vec::new();
            doc.for_each_layer(|l| v.push(l.id));
            v
        };
        for (id, img) in thumbs {
            let slot = self.thumbs.entry(id);
            match slot {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.get_mut().set(img, egui::TextureOptions::LINEAR)
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(ctx.load_texture(format!("thumb-{id}"), img, egui::TextureOptions::LINEAR));
                }
            }
        }
        for (id, img) in masks {
            match self.mask_thumbs.entry(id) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.get_mut().set(img, egui::TextureOptions::LINEAR)
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(ctx.load_texture(format!("mask-{id}"), img, egui::TextureOptions::LINEAR));
                }
            }
        }
        self.thumbs.retain(|id, _| live.contains(id));
        let doc = self.editor.doc();
        self.mask_thumbs
            .retain(|id, _| doc.layer(*id).is_some_and(|l| l.mask.is_some()));
    }

    /// Upload a preview of `doc` for the given area (or the whole canvas).
    fn preview(&mut self, ctx: &egui::Context, doc: &Document, area: Option<Rect>) {
        match (area, self.canvas_tex.as_mut()) {
            (Some(r), Some(tex)) => {
                let r = r.intersect(&doc.canvas());
                if !r.is_empty() {
                    let patch = nge_render::composite_rect(doc, r).to_raster(r);
                    tex.set_partial(
                        [r.x as usize, r.y as usize],
                        raster_to_image(&patch),
                        nearest_when_zoomed(),
                    );
                }
            }
            _ => {
                let img = composite_image(doc);
                upload(&mut self.canvas_tex, ctx, "canvas", img, nearest_when_zoomed());
            }
        }
    }

    // ---- shortcuts --------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Modifiers as M;
        if ctx.wants_keyboard_input() {
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
        }
        let (redo, invert, undo, all, none, save, open, xform, group) = ctx.input_mut(|i| {
            (
                i.consume_key(M::COMMAND | M::SHIFT, Key::Z) || i.consume_key(M::COMMAND, Key::Y),
                i.consume_key(M::COMMAND | M::SHIFT, Key::I),
                i.consume_key(M::COMMAND, Key::Z),
                i.consume_key(M::COMMAND, Key::A),
                i.consume_key(M::COMMAND, Key::D),
                i.consume_key(M::COMMAND, Key::S),
                i.consume_key(M::COMMAND, Key::O),
                i.consume_key(M::COMMAND, Key::T),
                i.consume_key(M::COMMAND, Key::G),
            )
        });
        if redo {
            self.redo();
        }
        if undo {
            self.undo();
        }
        if invert {
            self.run(&InvertSelection);
        }
        if all {
            self.run(&SetSelection {
                selection: Some(Selection::all()),
            });
        }
        if none {
            self.run(&SetSelection { selection: None });
        }
        if save {
            match self.path.clone() {
                Some(p) => self.save_path(&p.to_string_lossy()),
                None => self.dialog = Some(Dialog::Save("untitled.nge".into())),
            }
        }
        if open {
            self.dialog = Some(Dialog::Open(String::new()));
        }
        if xform {
            self.begin_free_transform();
        }
        if group {
            self.group_selected();
        }
        let (fill, clear) = ctx.input(|i| {
            (
                i.modifiers.shift && i.key_pressed(Key::F5),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
            )
        });
        if fill && self.active_is_pixel() {
            self.fill_active();
        }
        if clear && self.active_is_pixel() {
            self.clear_active();
        }
        let (tool, bigger, smaller, fit, actual) = ctx.input(|i| {
            if i.modifiers.command || i.modifiers.alt {
                return (None, false, false, false, false);
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
            } else if i.key_pressed(Key::H) {
                Some(Tool::Hand)
            } else {
                None
            };
            (
                tool,
                i.key_pressed(Key::CloseBracket),
                i.key_pressed(Key::OpenBracket),
                i.key_pressed(Key::Num0),
                i.key_pressed(Key::Num1),
            )
        });
        if let Some(t) = tool {
            self.tool = t;
        }
        if bigger {
            self.brush.radius = (self.brush.radius * 1.25).min(200.0);
        }
        if smaller {
            self.brush.radius = (self.brush.radius / 1.25).max(1.0);
        }
        if fit {
            self.view_cmd = Some(ViewCmd::Fit);
        }
        if actual {
            self.view_cmd = Some(ViewCmd::Actual);
        }
    }

    // ---- menu bar ----------------------------------------------------------------------

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New...").clicked() {
                        self.dialog = Some(Dialog::New(1200, 800));
                        ui.close_menu();
                    }
                    if ui
                        .button("Open...   Ctrl+O")
                        .on_hover_text(".nge project, .psd, .png, .jpg")
                        .clicked()
                    {
                        self.dialog = Some(Dialog::Open(String::new()));
                        ui.close_menu();
                    }
                    if ui.button("Open image (PNG/JPEG)...").clicked() {
                        self.dialog = Some(Dialog::OpenImage(String::new()));
                        ui.close_menu();
                    }
                    if ui.button("Place image as layer...").clicked() {
                        self.dialog = Some(Dialog::PlaceImage(String::new()));
                        ui.close_menu();
                    }
                    if ui.button("Open demo document").clicked() {
                        match nge_core::demo::build(1200, 800) {
                            Ok(ed) => self.set_doc(ed, None),
                            Err(e) => self.status = e.to_string(),
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Save project...   Ctrl+S").clicked() {
                        let p = self
                            .path
                            .as_ref()
                            .map_or("untitled.nge".to_string(), |p| p.to_string_lossy().into_owned());
                        self.dialog = Some(Dialog::Save(p));
                        ui.close_menu();
                    }
                    if ui.button("Export PNG...").clicked() {
                        self.dialog = Some(Dialog::Export("export.png".into()));
                        ui.close_menu();
                    }
                    if ui.button("Export JPEG...").clicked() {
                        self.dialog = Some(Dialog::ExportJpeg("export.jpg".into(), 90));
                        ui.close_menu();
                    }
                    if ui.button("Export Photoshop PSD...").clicked() {
                        self.dialog = Some(Dialog::ExportPsd("export.psd".into()));
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui
                        .add_enabled(self.editor.can_undo(), egui::Button::new("Undo   Ctrl+Z"))
                        .clicked()
                    {
                        self.undo();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(self.editor.can_redo(), egui::Button::new("Redo   Ctrl+Shift+Z"))
                        .clicked()
                    {
                        self.redo();
                        ui.close_menu();
                    }
                    ui.separator();
                    let pixel = self.active_is_pixel();
                    if ui
                        .add_enabled(pixel, egui::Button::new("Free transform   Ctrl+T"))
                        .clicked()
                    {
                        self.begin_free_transform();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(pixel, egui::Button::new("Fill with brush colour   Shift+F5"))
                        .clicked()
                    {
                        self.fill_active();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(pixel, egui::Button::new("Clear   Delete"))
                        .clicked()
                    {
                        self.clear_active();
                        ui.close_menu();
                    }
                });
                ui.menu_button("Image", |ui| {
                    let doc = self.editor.doc();
                    let (w, h) = (doc.width, doc.height);
                    let crop_rect = doc.selection.as_ref().map(|s| s.tight_bounds(doc.canvas()));
                    if ui
                        .add_enabled(
                            crop_rect.is_some_and(|r| !r.is_empty()),
                            egui::Button::new("Crop to selection"),
                        )
                        .clicked()
                    {
                        if let Some(rect) = crop_rect {
                            self.run(&CropDocument { rect });
                        }
                        ui.close_menu();
                    }
                    if ui.button("Canvas size...").clicked() {
                        self.dialog = Some(Dialog::CanvasSize(w, h, (0.5, 0.5)));
                        ui.close_menu();
                    }
                    if ui.button("Image size...").clicked() {
                        self.dialog = Some(Dialog::ImageSize(w, h, true));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Rotate 90° clockwise").clicked() {
                        self.run(&RotateImage { quarter_turns: 1 });
                        self.view_cmd = Some(ViewCmd::Fit);
                        ui.close_menu();
                    }
                    if ui.button("Rotate 90° counter-clockwise").clicked() {
                        self.run(&RotateImage { quarter_turns: -1 });
                        self.view_cmd = Some(ViewCmd::Fit);
                        ui.close_menu();
                    }
                    if ui.button("Rotate 180°").clicked() {
                        self.run(&RotateImage { quarter_turns: 2 });
                        ui.close_menu();
                    }
                    if ui.button("Flip image horizontal").clicked() {
                        self.run(&FlipImage { horizontal: true });
                        ui.close_menu();
                    }
                    if ui.button("Flip image vertical").clicked() {
                        self.run(&FlipImage { horizontal: false });
                        ui.close_menu();
                    }
                });
                ui.menu_button("Select", |ui| {
                    if ui.button("All   Ctrl+A").clicked() {
                        self.run(&SetSelection {
                            selection: Some(Selection::all()),
                        });
                        ui.close_menu();
                    }
                    if ui.button("None   Ctrl+D").clicked() {
                        self.run(&SetSelection { selection: None });
                        ui.close_menu();
                    }
                    if ui.button("Invert   Ctrl+Shift+I").clicked() {
                        self.run(&InvertSelection);
                        ui.close_menu();
                    }
                    ui.separator();
                    let has_sel = self.editor.doc().selection.is_some();
                    if ui.add_enabled(has_sel, egui::Button::new("Feather")).clicked() {
                        self.run(&FeatherSelection { radius: self.feather });
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            has_sel && self.active.is_some(),
                            egui::Button::new("Mask active layer"),
                        )
                        .clicked()
                    {
                        if let Some(l) = self.active {
                            self.run(&MaskFromSelection { layer: l });
                        }
                        ui.close_menu();
                    }
                });
                ui.menu_button("Layer", |ui| {
                    if ui.button("New pixel layer").clicked() {
                        self.add_pixel_layer();
                        ui.close_menu();
                    }
                    ui.menu_button("New adjustment layer", |ui| {
                        for (name, adj) in adjustment_presets() {
                            if ui.button(name).clicked() {
                                self.add_adjustment(adj);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button("New live filter layer", |ui| {
                        for (name, f) in filter_presets() {
                            if ui.button(name).clicked() {
                                self.add_filter_layer(f);
                                ui.close_menu();
                            }
                        }
                    });
                    if ui.button("Delete layer").clicked() {
                        self.delete_active();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Group   Ctrl+G").clicked() {
                        self.group_selected();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(self.active_is_group(), egui::Button::new("Ungroup"))
                        .clicked()
                    {
                        self.ungroup_active();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Move up").clicked() {
                        self.reorder_active(1);
                        ui.close_menu();
                    }
                    if ui.button("Move down").clicked() {
                        self.reorder_active(-1);
                        ui.close_menu();
                    }
                    ui.separator();
                    let has_layer = self.active.is_some();
                    let has_mask = self.active_has_mask();
                    if ui
                        .add_enabled(has_layer && !has_mask, egui::Button::new("Add mask"))
                        .clicked()
                    {
                        if let Some(l) = self.active {
                            self.run(&AddMask { layer: l });
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(has_mask, egui::Button::new("Remove mask"))
                        .clicked()
                    {
                        if let Some(l) = self.active {
                            self.run(&RemoveMask { layer: l });
                            self.editing_mask = false;
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    let pixel = self.active_is_pixel();
                    if ui
                        .add_enabled(pixel, egui::Button::new("Flip horizontal"))
                        .clicked()
                    {
                        self.flip_active(true);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(pixel, egui::Button::new("Flip vertical"))
                        .clicked()
                    {
                        self.flip_active(false);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Filter", |ui| {
                    let pixel = self.active_is_pixel();
                    if ui
                        .add_enabled(pixel, egui::Button::new("Gaussian blur..."))
                        .clicked()
                    {
                        self.dialog = Some(Dialog::Filter(Filter::GaussianBlur { radius: 8.0 }));
                        ui.close_menu();
                    }
                    if ui.add_enabled(pixel, egui::Button::new("Box blur...")).clicked() {
                        self.dialog = Some(Dialog::Filter(Filter::BoxBlur { radius: 5.0 }));
                        ui.close_menu();
                    }
                    if ui.add_enabled(pixel, egui::Button::new("Sharpen...")).clicked() {
                        self.dialog = Some(Dialog::Filter(Filter::Sharpen {
                            amount: 1.0,
                            radius: 2.0,
                        }));
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("Live (non-destructive) filter layers")
                            .weak()
                            .small(),
                    );
                    for (name, f) in filter_presets() {
                        if ui.button(format!("{name} layer")).clicked() {
                            self.add_filter_layer(f);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Fit on screen   0").clicked() {
                        self.view_cmd = Some(ViewCmd::Fit);
                        ui.close_menu();
                    }
                    if ui.button("Actual pixels   1").clicked() {
                        self.view_cmd = Some(ViewCmd::Actual);
                        ui.close_menu();
                    }
                });
            });
        });
    }

    // ---- options bar ----------------------------------------------------------------

    fn options_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("options").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(x) = self.xform.clone() {
                    ui.label(RichText::new("Free Transform").strong());
                    ui.separator();
                    ui.label(format!("Scale {:.0}%", x.scale * 100.0));
                    ui.label(format!("Rotate {:.1}°", x.angle.to_degrees()));
                    ui.label(format!("Offset {:.0}, {:.0}", x.dx, x.dy));
                    ui.separator();
                    if ui.button("Apply   Enter").clicked() {
                        self.commit_free_transform();
                    }
                    if ui.button("Cancel   Esc").clicked() {
                        self.cancel_free_transform();
                    }
                    return;
                }
                ui.label(RichText::new(self.tool.name()).strong());
                if self.editing_mask {
                    ui.label(
                        RichText::new("MASK")
                            .small()
                            .strong()
                            .color(Color32::BLACK)
                            .background_color(Color32::from_rgb(230, 200, 90)),
                    );
                }
                ui.separator();
                match self.tool {
                    Tool::Move => {
                        ui.label(RichText::new("Drag to move the active layer").weak());
                        if ui
                            .add_enabled(self.active_is_pixel(), egui::Button::new("Free transform"))
                            .clicked()
                        {
                            self.begin_free_transform();
                        }
                    }
                    Tool::Eyedropper => {
                        ui.label(RichText::new("Click to pick the brush colour from the image").weak());
                        egui::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                    }
                    Tool::Bucket | Tool::Wand => {
                        let mut tol = self.tolerance * 100.0;
                        ui.label("Tolerance");
                        if ui
                            .add(egui::Slider::new(&mut tol, 0.0..=100.0).suffix("%"))
                            .changed()
                        {
                            self.tolerance = tol / 100.0;
                        }
                        ui.checkbox(&mut self.contiguous, "Contiguous");
                        ui.checkbox(&mut self.sample_merged, "Sample all layers");
                        if self.tool == Tool::Bucket {
                            egui::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                            let mut op = self.brush.color[3] * 100.0;
                            ui.label("Opacity");
                            if ui
                                .add(egui::Slider::new(&mut op, 1.0..=100.0).suffix("%"))
                                .changed()
                            {
                                self.brush.color[3] = op / 100.0;
                            }
                        } else {
                            ui.separator();
                            ui.selectable_value(&mut self.select_op, CombineOp::Replace, "New");
                            ui.selectable_value(&mut self.select_op, CombineOp::Union, "Add");
                            ui.selectable_value(&mut self.select_op, CombineOp::Subtract, "Subtract");
                            ui.selectable_value(&mut self.select_op, CombineOp::Intersect, "Intersect");
                        }
                    }
                    Tool::Text => {
                        if let (Some(id), Some(t)) = (self.active, self.active_text()) {
                            self.text_controls(ui, id, t, false);
                            if ui.button("Rasterize").clicked() {
                                self.run(&RasterizeLayer { layer: id });
                            }
                        } else {
                            ui.add(
                                egui::Slider::new(&mut self.text_size, 6.0..=400.0)
                                    .logarithmic(true)
                                    .suffix(" px")
                                    .text("Size"),
                            );
                            ui.checkbox(&mut self.text_bold, "Bold");
                            egui::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                            ui.label(RichText::new("Click on the canvas to add text").weak());
                        }
                    }
                    Tool::Gradient => {
                        ui.selectable_value(&mut self.gradient_kind, GradientKind::Linear, "Linear");
                        ui.selectable_value(&mut self.gradient_kind, GradientKind::Radial, "Radial");
                        ui.separator();
                        ui.label("From");
                        egui::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                        ui.label("To");
                        egui::color_picker::color_edit_button_rgb(ui, &mut self.bg_rgb);
                        ui.checkbox(&mut self.gradient_to_transparent, "To transparent");
                        ui.label(RichText::new("Drag on the canvas").weak());
                    }
                    Tool::Brush | Tool::Eraser | Tool::Clone => {
                        if self.tool == Tool::Clone {
                            let picking = self.clone_picking || self.clone_source.is_none();
                            if ui
                                .selectable_label(picking, "Pick source")
                                .on_hover_text("Next click sets the clone source (or Alt+click)")
                                .clicked()
                            {
                                self.clone_picking = true;
                            }
                            match self.clone_source {
                                Some((x, y)) => ui.label(format!("Source {:.0}, {:.0}", x, y)),
                                None => ui.label(RichText::new("No source yet").weak()),
                            };
                            ui.checkbox(&mut self.sample_merged, "Sample all layers");
                            ui.separator();
                        }
                        let mut size = self.brush.radius * 2.0;
                        ui.label("Size");
                        if ui
                            .add(
                                egui::Slider::new(&mut size, 2.0..=400.0)
                                    .logarithmic(true)
                                    .suffix(" px"),
                            )
                            .changed()
                        {
                            self.brush.radius = size / 2.0;
                        }
                        let mut hard = self.brush.hardness * 100.0;
                        ui.label("Hardness");
                        if ui
                            .add(egui::Slider::new(&mut hard, 0.0..=100.0).suffix("%"))
                            .changed()
                        {
                            self.brush.hardness = hard / 100.0;
                        }
                        let mut op = self.brush.color[3] * 100.0;
                        ui.label("Opacity");
                        if ui
                            .add(egui::Slider::new(&mut op, 1.0..=100.0).suffix("%"))
                            .changed()
                        {
                            self.brush.color[3] = op / 100.0;
                        }
                        if self.tool == Tool::Brush {
                            egui::color_picker::color_edit_button_rgb(ui, &mut self.brush_rgb);
                            if self.editing_mask {
                                if ui.small_button("White").clicked() {
                                    self.brush_rgb = [1.0; 3];
                                }
                                if ui.small_button("Black").clicked() {
                                    self.brush_rgb = [0.0; 3];
                                }
                            }
                        }
                    }
                    Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::PolyLasso => {
                        if self.tool == Tool::PolyLasso {
                            ui.label(RichText::new("Click to add points, double-click to close").weak());
                            if !self.lasso.is_empty() {
                                if ui.button("Close").clicked() {
                                    self.finish_polygon(ctx);
                                }
                                if ui.button("Cancel").clicked() {
                                    self.lasso.clear();
                                }
                            }
                            ui.separator();
                        }
                        ui.selectable_value(&mut self.select_op, CombineOp::Replace, "New");
                        ui.selectable_value(&mut self.select_op, CombineOp::Union, "Add");
                        ui.selectable_value(&mut self.select_op, CombineOp::Subtract, "Subtract");
                        ui.selectable_value(&mut self.select_op, CombineOp::Intersect, "Intersect");
                        ui.separator();
                        ui.label("Feather");
                        ui.add(egui::Slider::new(&mut self.feather, 0.0..=100.0).suffix(" px"));
                        let has_sel = self.editor.doc().selection.is_some();
                        if ui.add_enabled(has_sel, egui::Button::new("Apply")).clicked() {
                            self.run(&FeatherSelection { radius: self.feather });
                        }
                        ui.label(RichText::new("Shift adds, Alt subtracts").weak());
                    }
                    Tool::Hand => {
                        ui.label(RichText::new("Drag to pan, scroll to zoom").weak());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("100%").clicked() {
                        self.view_cmd = Some(ViewCmd::Actual);
                    }
                    if ui.button("Fit").clicked() {
                        self.view_cmd = Some(ViewCmd::Fit);
                    }
                });
            });
        });
    }

    fn tool_palette(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("tools")
            .exact_width(58.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(Color32::from_gray(28)).inner_margin(9.0))
            .show(ctx, |ui| {
                for tool in Tool::ALL {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
                    let bg = if self.tool == tool {
                        ACCENT
                    } else if resp.hovered() {
                        Color32::from_gray(64)
                    } else {
                        Color32::from_gray(44)
                    };
                    ui.painter().rect_filled(rect, 6.0, bg);
                    draw_icon(ui.painter(), rect.shrink(10.0), tool);
                    if resp.on_hover_text(tool.tip()).clicked() {
                        self.tool = tool;
                        self.lasso.clear();
                        if tool != Tool::Move {
                            self.cancel_free_transform();
                        }
                    }
                }
            });
    }

    // ---- right panel ---------------------------------------------------------------

    fn side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("side")
            .default_width(330.0)
            .min_width(290.0)
            .show(ctx, |ui| {
                self.layers_ui(ui);
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("props")
                    .max_height(330.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.properties_ui(ui));
                ui.separator();
                self.history_ui(ui);
            });
    }

    fn layer_rows(&self) -> Vec<LayerRow> {
        fn walk(layers: &[Layer], depth: usize, out: &mut Vec<LayerRow>) {
            for l in layers.iter().rev() {
                let kind = match l.content {
                    LayerContent::Pixel(_) => Kind::Pixel,
                    LayerContent::Group(_) => Kind::Group,
                    LayerContent::Adjustment(_) => Kind::Adjustment,
                    LayerContent::Filter(_) => Kind::Filter,
                    LayerContent::Text(_) => Kind::Text,
                };
                out.push(LayerRow {
                    id: l.id,
                    name: l.name.clone(),
                    visible: l.visible,
                    opacity: l.opacity,
                    kind,
                    masked: l.mask.is_some(),
                    mask_enabled: l.mask.as_ref().is_some_and(|m| m.enabled),
                    depth,
                    collapsed: l.collapsed,
                });
                if let (Some(children), false) = (l.children(), l.collapsed) {
                    walk(children, depth + 1, out);
                }
            }
        }
        let mut rows = Vec::new();
        walk(self.editor.doc().layers(), 0, &mut rows);
        rows
    }

    fn layers_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "LAYERS");
        let rows = self.layer_rows();
        let ctrl = ui.input(|i| i.modifiers.command);

        let mut toggle_vis = None;
        let mut select: Option<(LayerId, bool)> = None;
        let mut toggle_collapse = None;
        let mut rename_start = None;
        let mut rename_commit = None;
        let mut rename_cancel = false;
        let mut mask_click = None;
        let mut renaming = self.renaming.take();

        egui::ScrollArea::vertical()
            .id_salt("layers")
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in &rows {
                    let selected = self.selected.contains(&row.id);
                    let is_active = Some(row.id) == self.active;
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 38.0), Sense::click());
                    let p = ui.painter();
                    if is_active {
                        p.rect_filled(rect, 4.0, ACCENT);
                    } else if selected {
                        p.rect_filled(rect, 4.0, Color32::from_rgb(45, 70, 115));
                    } else if resp.hovered() {
                        p.rect_filled(rect, 4.0, Color32::from_gray(50));
                    }
                    let mut x = rect.min.x + 6.0 + row.depth as f32 * 16.0;
                    let cy = rect.center().y;

                    // visibility checkbox
                    let vis_rect = egui::Rect::from_center_size(egui::pos2(x + 8.0, cy), Vec2::splat(14.0));
                    p.rect_stroke(vis_rect, 2.0, Stroke::new(1.0, Color32::from_gray(150)));
                    if row.visible {
                        p.circle_filled(vis_rect.center(), 4.0, Color32::WHITE);
                    }
                    if resp.clicked()
                        && resp
                            .interact_pointer_pos()
                            .is_some_and(|q| vis_rect.expand(3.0).contains(q))
                    {
                        toggle_vis = Some((row.id, !row.visible));
                    }
                    x += 22.0;

                    // group disclosure triangle
                    if row.kind == Kind::Group {
                        let tri_rect =
                            egui::Rect::from_center_size(egui::pos2(x + 6.0, cy), Vec2::splat(12.0));
                        let c = Color32::from_gray(210);
                        let pts = if row.collapsed {
                            vec![
                                egui::pos2(tri_rect.min.x + 2.0, tri_rect.min.y),
                                egui::pos2(tri_rect.max.x, tri_rect.center().y),
                                egui::pos2(tri_rect.min.x + 2.0, tri_rect.max.y),
                            ]
                        } else {
                            vec![
                                egui::pos2(tri_rect.min.x, tri_rect.min.y + 2.0),
                                egui::pos2(tri_rect.max.x, tri_rect.min.y + 2.0),
                                egui::pos2(tri_rect.center().x, tri_rect.max.y),
                            ]
                        };
                        p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
                        if resp.clicked()
                            && resp
                                .interact_pointer_pos()
                                .is_some_and(|q| tri_rect.expand(4.0).contains(q))
                        {
                            toggle_collapse = Some((row.id, !row.collapsed));
                        }
                        x += 16.0;
                    }

                    // thumbnail
                    let t_rect = egui::Rect::from_min_size(
                        egui::pos2(x, cy - 15.0),
                        egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                    );
                    paint_thumb_bg(p, t_rect);
                    if let Some(tex) = self.thumbs.get(&row.id) {
                        p.image(
                            tex.id(),
                            t_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        badge(p, t_rect, row.kind);
                    }
                    p.rect_stroke(t_rect, 2.0, Stroke::new(1.0, Color32::from_gray(90)));
                    x += THUMB.0 as f32 + 6.0;

                    // mask thumbnail
                    if row.masked {
                        let m_rect = egui::Rect::from_min_size(
                            egui::pos2(x, cy - 15.0),
                            egui::vec2(THUMB.0 as f32, THUMB.1 as f32),
                        );
                        if let Some(tex) = self.mask_thumbs.get(&row.id) {
                            p.image(
                                tex.id(),
                                m_rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        let editing = is_active && self.editing_mask;
                        let col = if editing {
                            Color32::from_rgb(230, 200, 90)
                        } else if row.mask_enabled {
                            Color32::from_gray(90)
                        } else {
                            Color32::from_rgb(200, 70, 70)
                        };
                        p.rect_stroke(m_rect, 2.0, Stroke::new(if editing { 2.0 } else { 1.0 }, col));
                        if !row.mask_enabled {
                            p.line_segment(
                                [m_rect.left_top(), m_rect.right_bottom()],
                                Stroke::new(1.5, Color32::from_rgb(200, 70, 70)),
                            );
                        }
                        if resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| m_rect.contains(q)) {
                            mask_click = Some(row.id);
                        }
                        x += THUMB.0 as f32 + 6.0;
                    }

                    // name (or rename box)
                    if let Some((rid, text)) = renaming.as_mut() {
                        if *rid == row.id {
                            let edit_rect = egui::Rect::from_min_max(
                                egui::pos2(x, cy - 11.0),
                                egui::pos2(rect.max.x - 6.0, cy + 11.0),
                            );
                            let r = ui.put(edit_rect, egui::TextEdit::singleline(text));
                            r.request_focus();
                            let (enter, escape, click_away) = ui.input(|i| {
                                (
                                    i.key_pressed(Key::Enter),
                                    i.key_pressed(Key::Escape),
                                    i.pointer.any_pressed() && !r.hovered(),
                                )
                            });
                            if escape {
                                rename_cancel = true;
                            } else if enter || click_away || r.lost_focus() {
                                rename_commit = Some((row.id, text.clone()));
                            }
                            continue;
                        }
                    }
                    let text_col = if is_active {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(215)
                    };
                    p.text(
                        egui::pos2(x, cy),
                        Align2::LEFT_CENTER,
                        &row.name,
                        FontId::proportional(14.0),
                        text_col,
                    );
                    let mut extra = String::new();
                    if row.kind == Kind::Adjustment {
                        extra.push_str("adj  ");
                    }
                    if row.kind == Kind::Filter {
                        extra.push_str("live filter  ");
                    }
                    if row.kind == Kind::Text {
                        extra.push_str("text  ");
                    }
                    if row.opacity < 0.999 {
                        extra.push_str(&format!("{:.0}%", row.opacity * 100.0));
                    }
                    if !extra.is_empty() {
                        p.text(
                            rect.right_center() - egui::vec2(8.0, 0.0),
                            Align2::RIGHT_CENTER,
                            extra.trim_end(),
                            FontId::proportional(11.5),
                            if is_active {
                                Color32::from_gray(225)
                            } else {
                                Color32::from_gray(140)
                            },
                        );
                    }
                    if resp.double_clicked() {
                        rename_start = Some((row.id, row.name.clone()));
                    } else if resp.clicked() {
                        let on_control = resp
                            .interact_pointer_pos()
                            .is_some_and(|q| q.x < x - 2.0 && q.x > vis_rect.max.x + 2.0);
                        if !on_control
                            && !resp
                                .interact_pointer_pos()
                                .is_some_and(|q| vis_rect.expand(3.0).contains(q))
                        {
                            select = Some((row.id, ctrl));
                        }
                    }
                }
            });

        if rename_cancel {
            renaming = None;
        }
        if let Some((layer, name)) = rename_commit {
            renaming = None;
            self.run(&RenameLayer { layer, name });
        }
        if rename_start.is_some() {
            renaming = rename_start;
        }
        self.renaming = renaming;
        if let Some((id, add)) = select {
            if add {
                if let Some(i) = self.selected.iter().position(|x| *x == id) {
                    if self.selected.len() > 1 {
                        self.selected.remove(i);
                    }
                } else {
                    self.selected.push(id);
                }
                self.active = Some(id);
                self.editing_mask = false;
            } else {
                self.set_active(Some(id));
            }
        }
        if let Some(id) = mask_click {
            if Some(id) == self.active {
                self.editing_mask = !self.editing_mask;
            } else {
                self.set_active(Some(id));
                self.editing_mask = true;
            }
        }
        if let Some((layer, visible)) = toggle_vis {
            self.run(&SetVisible { layer, visible });
        }
        if let Some((layer, collapsed)) = toggle_collapse {
            self.run(&SetCollapsed { layer, collapsed });
        }

        let mut action: Option<&str> = None;
        let mut add_adj = None;
        let mut add_filter = None;
        ui.horizontal(|ui| {
            if ui.button("+ Layer").clicked() {
                action = Some("add");
            }
            ui.menu_button("+ Adjustment", |ui| {
                for (name, adj) in adjustment_presets() {
                    if ui.button(name).clicked() {
                        add_adj = Some(adj);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("+ Filter", |ui| {
                for (name, f) in filter_presets() {
                    if ui.button(name).clicked() {
                        add_filter = Some(f);
                        ui.close_menu();
                    }
                }
            });
            if ui
                .button("Group")
                .on_hover_text("Group selected layers (Ctrl+click to multi-select)")
                .clicked()
            {
                action = Some("group");
            }
            if ui
                .add_enabled(self.active_is_group(), egui::Button::new("Ungroup"))
                .clicked()
            {
                action = Some("ungroup");
            }
            if ui.button("Delete").clicked() {
                action = Some("delete");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Up").clicked() {
                action = Some("up");
            }
            if ui.button("Down").clicked() {
                action = Some("down");
            }
            let has_mask = self.active_has_mask();
            if ui
                .add_enabled(self.active.is_some() && !has_mask, egui::Button::new("Add mask"))
                .clicked()
            {
                action = Some("addmask");
            }
            if ui
                .add_enabled(has_mask, egui::Button::new("Remove mask"))
                .clicked()
            {
                action = Some("rmmask");
            }
            if has_mask {
                let enabled = self
                    .active_layer()
                    .and_then(|l| l.mask.as_ref())
                    .is_some_and(|m| m.enabled);
                let mut e = enabled;
                if ui.checkbox(&mut e, "On").changed() {
                    action = Some(if e { "maskon" } else { "maskoff" });
                }
            }
        });
        match action {
            Some("add") => self.add_pixel_layer(),
            Some("group") => self.group_selected(),
            Some("ungroup") => self.ungroup_active(),
            Some("delete") => self.delete_active(),
            Some("up") => self.reorder_active(1),
            Some("down") => self.reorder_active(-1),
            Some("addmask") => {
                if let Some(l) = self.active {
                    self.run(&AddMask { layer: l });
                    self.editing_mask = true;
                }
            }
            Some("rmmask") => {
                if let Some(l) = self.active {
                    self.run(&RemoveMask { layer: l });
                    self.editing_mask = false;
                }
            }
            Some("maskon") | Some("maskoff") => {
                if let Some(l) = self.active {
                    self.run(&SetMaskEnabled {
                        layer: l,
                        enabled: action == Some("maskon"),
                    });
                }
            }
            _ => {}
        }
        if let Some(adj) = add_adj {
            self.add_adjustment(adj);
        }
        if let Some(f) = add_filter {
            self.add_filter_layer(f);
        }
    }

    fn properties_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "PROPERTIES");
        let Some(id) = self.active else {
            ui.label(RichText::new("No layer selected").weak());
            return;
        };
        let Some(layer) = self.editor.doc().layer(id) else {
            return;
        };
        let name = layer.name.clone();
        let blend = layer.blend;
        let adj = match &layer.content {
            LayerContent::Adjustment(a) => Some(a.clone()),
            _ => None,
        };
        let filt = match &layer.content {
            LayerContent::Filter(f) => Some(f.clone()),
            _ => None,
        };
        let mut opacity = layer.opacity * 100.0;
        ui.label(RichText::new(name).strong());

        let r = ui.add(
            egui::Slider::new(&mut opacity, 0.0..=100.0)
                .suffix("%")
                .text("Opacity"),
        );
        if r.changed() {
            self.run_coalescing(
                &SetOpacity {
                    layer: id,
                    opacity: opacity / 100.0,
                },
                &format!("opacity-{id}"),
            );
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            self.editor.end_coalescing();
        }

        let mut b = blend;
        egui::ComboBox::from_label("Blend mode")
            .selected_text(b.name())
            .show_ui(ui, |ui| {
                for m in BlendMode::ALL {
                    ui.selectable_value(&mut b, m, m.name());
                }
            });
        if b != blend {
            self.run(&SetBlendMode { layer: id, blend: b });
        }

        if let Some(adj) = adj {
            ui.add_space(4.0);
            ui.label(RichText::new(adj.name()).small().strong());
            self.adjustment_ui(ui, id, adj);
        } else if let Some(t) = self.active_text() {
            ui.add_space(4.0);
            ui.label(RichText::new("Text").small().strong());
            self.text_controls(ui, id, t, true);
            if ui.button("Rasterize").clicked() {
                self.run(&RasterizeLayer { layer: id });
            }
        } else if let Some(mut f) = filt {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("{} (live)", f.name())).small().strong());
            let before = f.clone();
            let mut finished = false;
            match &mut f {
                Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
                    let r = ui.add(
                        egui::Slider::new(radius, 0.5..=60.0)
                            .logarithmic(true)
                            .suffix(" px")
                            .text("Radius"),
                    );
                    finished |= r.drag_stopped() || (r.changed() && !r.dragged());
                }
                Filter::Sharpen { amount, radius } => {
                    finished |= slider_row(ui, "Amount", amount, 0.0..=5.0, "");
                    finished |= slider_row(ui, "Radius", radius, 0.5..=20.0, " px");
                }
            }
            if f != before {
                self.run_coalescing(&SetFilter { layer: id, filter: f }, &format!("filter-{id}"));
            }
            if finished {
                self.editor.end_coalescing();
            }
            ui.label(
                RichText::new("Applies to everything below; the pixels stay untouched.")
                    .weak()
                    .small(),
            );
        } else if self.active_is_pixel() {
            ui.add_space(4.0);
            ui.label(RichText::new("Transform").small().strong());
            ui.add(
                egui::Slider::new(&mut self.xform_scale, 10.0..=400.0)
                    .suffix("%")
                    .text("Scale"),
            );
            ui.add(
                egui::Slider::new(&mut self.xform_angle, -180.0..=180.0)
                    .suffix("°")
                    .text("Rotate"),
            );
            let mut apply = false;
            let mut flip = None;
            let mut free = false;
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply = true;
                }
                if ui.button("Flip H").clicked() {
                    flip = Some(true);
                }
                if ui.button("Flip V").clicked() {
                    flip = Some(false);
                }
                if ui.button("Free transform").clicked() {
                    free = true;
                }
            });
            if apply {
                let s = self.xform_scale / 100.0;
                let a = self.xform_angle.to_radians();
                if let Some(cmd) = TransformLayer::around_center(self.editor.doc(), id, s, s, a) {
                    self.run(&cmd);
                    self.xform_scale = 100.0;
                    self.xform_angle = 0.0;
                } else {
                    self.status = "Layer has no pixels to transform".into();
                }
            }
            if let Some(h) = flip {
                self.flip_active(h);
            }
            if free {
                self.begin_free_transform();
            }
        }
    }

    fn adjustment_ui(&mut self, ui: &mut egui::Ui, id: LayerId, mut adj: Adjustment) {
        let before = adj.clone();
        let mut finished = false;
        match &mut adj {
            Adjustment::Invert => {
                ui.label(RichText::new("This adjustment has no settings.").weak());
            }
            Adjustment::BrightnessContrast { brightness, contrast } => {
                finished |= slider_row(ui, "Brightness", brightness, -1.0..=1.0, "");
                finished |= slider_row(ui, "Contrast", contrast, -1.0..=1.0, "");
            }
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                finished |= slider_row(ui, "Hue", hue, -180.0..=180.0, "°");
                finished |= slider_row(ui, "Saturation", saturation, -1.0..=1.0, "");
                finished |= slider_row(ui, "Lightness", lightness, -1.0..=1.0, "");
            }
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => {
                finished |= slider_row(ui, "Input black", in_black, 0.0..=1.0, "");
                finished |= slider_row(ui, "Input white", in_white, 0.0..=1.0, "");
                finished |= slider_row(ui, "Gamma", gamma, 0.1..=4.0, "");
                finished |= slider_row(ui, "Output black", out_black, 0.0..=1.0, "");
                finished |= slider_row(ui, "Output white", out_white, 0.0..=1.0, "");
            }
            Adjustment::Curves { points } => {
                finished |= curve_editor(ui, points, &mut self.curve_drag);
            }
            Adjustment::BlackWhite { red, green, blue } => {
                finished |= slider_row(ui, "Red", red, 0.0..=1.0, "");
                finished |= slider_row(ui, "Green", green, 0.0..=1.0, "");
                finished |= slider_row(ui, "Blue", blue, 0.0..=1.0, "");
            }
            Adjustment::Exposure {
                exposure,
                offset,
                gamma,
            } => {
                finished |= slider_row(ui, "Exposure", exposure, -4.0..=4.0, " EV");
                finished |= slider_row(ui, "Offset", offset, -0.5..=0.5, "");
                finished |= slider_row(ui, "Gamma", gamma, 0.1..=4.0, "");
            }
            Adjustment::ColorBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.cb_tone, 0, "Shadows");
                    ui.selectable_value(&mut self.cb_tone, 1, "Midtones");
                    ui.selectable_value(&mut self.cb_tone, 2, "Highlights");
                });
                let tone = match self.cb_tone {
                    0 => shadows,
                    1 => midtones,
                    _ => highlights,
                };
                finished |= slider_row(ui, "Cyan ↔ Red", &mut tone[0], -1.0..=1.0, "");
                finished |= slider_row(ui, "Magenta ↔ Green", &mut tone[1], -1.0..=1.0, "");
                finished |= slider_row(ui, "Yellow ↔ Blue", &mut tone[2], -1.0..=1.0, "");
                if ui.checkbox(preserve_luminosity, "Preserve luminosity").changed() {
                    finished = true;
                }
            }
            Adjustment::Vibrance { vibrance, saturation } => {
                finished |= slider_row(ui, "Vibrance", vibrance, -1.0..=1.0, "");
                finished |= slider_row(ui, "Saturation", saturation, -1.0..=1.0, "");
            }
            Adjustment::Threshold { level } => {
                finished |= slider_row(ui, "Level", level, 0.0..=1.0, "");
            }
            Adjustment::Posterize { levels } => {
                let mut v = *levels as f32;
                let r = ui.add(egui::Slider::new(&mut v, 2.0..=32.0).integer().text("Levels"));
                *levels = v.round() as u32;
                finished |= r.drag_stopped() || (r.changed() && !r.dragged());
            }
        }
        if adj != before {
            self.run_coalescing(
                &SetAdjustment {
                    layer: id,
                    adjustment: adj,
                },
                &format!("adj-{id}"),
            );
        }
        if finished {
            self.editor.end_coalescing();
        }
    }

    fn history_ui(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "HISTORY");
        let items: Vec<String> = self.editor.history().iter().map(|s| s.to_string()).collect();
        let redo: Vec<String> = self.editor.redo_history().iter().map(|s| s.to_string()).collect();
        let current = items.len();
        let mut jump = None;
        let row = |ui: &mut egui::Ui, idx: usize, text: &str, state: u8| -> bool {
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 20.0), Sense::click());
            if state == 1 {
                ui.painter()
                    .rect_filled(rect, 3.0, Color32::from_rgb(40, 62, 100));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, Color32::from_gray(50));
            }
            let col = match state {
                1 => Color32::from_rgb(170, 205, 255),
                2 => Color32::from_gray(95),
                _ => Color32::from_gray(160),
            };
            ui.painter().text(
                rect.left_center() + egui::vec2(6.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{idx:>2}  {text}"),
                FontId::proportional(13.0),
                col,
            );
            resp.clicked()
        };
        egui::ScrollArea::vertical()
            .id_salt("history")
            .max_height(260.0)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if row(ui, 0, "Original", u8::from(current == 0)) {
                    jump = Some(0);
                }
                for (i, label) in items.iter().enumerate() {
                    if row(ui, i + 1, label, u8::from(i + 1 == current)) {
                        jump = Some(i + 1);
                    }
                }
                for (k, label) in redo.iter().enumerate() {
                    if row(ui, current + k + 1, label, 2) {
                        jump = Some(current + k + 1);
                    }
                }
            });
        if let Some(n) = jump {
            self.editor.jump_to(n);
            self.mark(None);
            self.fix_active();
            self.status = format!("Jumped to history step {n}");
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let doc = self.editor.doc();
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                ui.separator();
                ui.label(format!("{} × {} px", doc.width, doc.height));
                ui.separator();
                match self.cursor_doc {
                    Some((x, y)) => ui.label(format!("x {x}  y {y}")),
                    None => ui.label("x –  y –"),
                };
                ui.separator();
                match &doc.selection {
                    Some(s) => {
                        let b = s.bounds_within(doc.canvas());
                        ui.label(format!("Selection ≈ {} × {} at {}, {}", b.w, b.h, b.x, b.y));
                    }
                    None => {
                        ui.label(RichText::new("No selection").weak());
                    }
                }
                ui.separator();
                ui.label(format!("{} layers", doc.layer_count()));
                if self.editing_mask {
                    ui.separator();
                    ui.label(RichText::new("Editing mask").color(Color32::from_rgb(230, 200, 90)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&self.status).weak());
                });
            });
        });
    }

    // ---- canvas ---------------------------------------------------------------------

    fn zoom_at(&mut self, rect: egui::Rect, p: Pos2, factor: f32) {
        let old = self.zoom;
        let new = (old * factor).clamp(0.05, 32.0);
        let origin = rect.min + self.pan;
        let doc_pt = (p - origin) / old;
        self.zoom = new;
        self.pan = p - rect.min - doc_pt * new;
    }

    fn apply_view_cmd(&mut self, rect: egui::Rect) {
        let Some(cmd) = self.view_cmd.take() else { return };
        let doc = self.editor.doc();
        let size = Vec2::new(doc.width as f32, doc.height as f32);
        self.zoom = match cmd {
            ViewCmd::Fit => ((rect.width() - 80.0) / size.x)
                .min((rect.height() - 80.0) / size.y)
                .clamp(0.05, 32.0),
            ViewCmd::Actual => 1.0,
        };
        self.pan = (rect.size() - size * self.zoom) / 2.0;
    }

    fn canvas(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::from_gray(45)))
            .show(ctx, |ui| {
                let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
                let rect = resp.rect;
                let painter = painter.with_clip_rect(rect);
                self.apply_view_cmd(rect);

                if resp.hovered() {
                    let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                    let pinch = ctx.input(|i| i.zoom_delta());
                    let factor = (scroll * 0.004).exp() * pinch;
                    if (factor - 1.0).abs() > 1e-4 {
                        if let Some(p) = resp.hover_pos() {
                            self.zoom_at(rect, p, factor);
                        }
                    }
                }
                if resp.dragged_by(egui::PointerButton::Middle)
                    || (self.tool == Tool::Hand && resp.dragged_by(egui::PointerButton::Primary))
                {
                    self.pan += resp.drag_delta();
                }

                let (dw, dh) = (self.editor.doc().width as f32, self.editor.doc().height as f32);
                let origin = rect.min + self.pan;
                let zoom = self.zoom;
                let doc_rect = egui::Rect::from_min_size(origin, Vec2::new(dw, dh) * zoom);
                let to_doc = move |p: Pos2| ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom);
                let to_screen = move |x: f32, y: f32| egui::pos2(origin.x + x * zoom, origin.y + y * zoom);

                self.cursor_doc = resp.hover_pos().and_then(|p| {
                    let (x, y) = to_doc(p);
                    (x >= 0.0 && y >= 0.0 && x < dw && y < dh).then_some((x as i32, y as i32))
                });

                if self.xform.is_some() {
                    self.handle_xform(ctx, &resp, to_doc, to_screen);
                } else {
                    self.handle_tool(ctx, &resp, to_doc);
                }

                painter.rect_filled(
                    doc_rect.translate(egui::vec2(0.0, 3.0)).expand(2.0),
                    2.0,
                    Color32::from_black_alpha(90),
                );
                paint_checker(&painter, doc_rect, rect);
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                if let Some(tex) = &self.canvas_tex {
                    painter.image(tex.id(), doc_rect, uv, Color32::WHITE);
                }
                if let Some(tex) = &self.overlay_tex {
                    painter.image(tex.id(), doc_rect, uv, Color32::WHITE);
                }
                painter.rect_stroke(doc_rect, 0.0, Stroke::new(1.0, Color32::from_gray(90)));

                if let Some(x) = &self.xform {
                    paint_xform_box(&painter, x, to_screen);
                } else {
                    self.paint_tool_overlay(ctx, &painter, &resp);
                }
            });
    }

    fn handle_xform(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
        to_screen: impl Fn(f32, f32) -> Pos2,
    ) {
        let primary = egui::PointerButton::Primary;
        let Some(mut x) = self.xform.clone() else { return };
        let (cx, cy) = x.center();
        let centre_s = to_screen(cx + x.dx, cy + x.dy);

        let hit = |q: Pos2| -> Handle {
            for (i, (px, py)) in x.corners().iter().enumerate() {
                if to_screen(*px, *py).distance(q) <= 10.0 {
                    return Handle::Corner(i);
                }
            }
            let poly: Vec<Pos2> = x.corners().iter().map(|(px, py)| to_screen(*px, *py)).collect();
            if point_in_convex(&poly, q) {
                Handle::Inside
            } else {
                Handle::Rotate
            }
        };
        if let Some(q) = resp.hover_pos() {
            ctx.set_cursor_icon(match hit(q) {
                Handle::Corner(_) => egui::CursorIcon::ResizeNwSe,
                Handle::Inside => egui::CursorIcon::Move,
                Handle::Rotate => egui::CursorIcon::Alias,
            });
        }
        if resp.drag_started_by(primary) {
            if let Some(q) = ctx.input(|i| i.pointer.press_origin()) {
                self.drag = Some(DragKind::Xform(hit(q)));
                self.drag_start = Some(q);
                x.base = (x.scale, x.angle, x.dx, x.dy);
            }
        }
        let mut changed = false;
        if let (Some(DragKind::Xform(h)), true) = (self.drag, resp.dragged_by(primary)) {
            if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                match h {
                    Handle::Corner(_) => {
                        let d0 = a.distance(centre_s).max(1.0);
                        let d1 = b.distance(centre_s);
                        x.scale = (x.base.0 * d1 / d0).clamp(0.02, 50.0);
                    }
                    Handle::Inside => {
                        let (ax, ay) = to_doc(a);
                        let (bx, by) = to_doc(b);
                        x.dx = x.base.2 + (bx - ax);
                        x.dy = x.base.3 + (by - ay);
                    }
                    Handle::Rotate => {
                        let a0 = (a.y - centre_s.y).atan2(a.x - centre_s.x);
                        let a1 = (b.y - centre_s.y).atan2(b.x - centre_s.x);
                        x.angle = x.base.1 + (a1 - a0);
                    }
                }
                changed = true;
            }
        }
        if resp.drag_stopped() {
            if let Some(DragKind::Xform(_)) = self.drag {
                self.drag = None;
                self.drag_start = None;
            }
        }
        if changed {
            // Preview: transform a copy-on-write clone and redraw the union of the old and new boxes.
            let mut preview = self.editor.doc().clone();
            let cmd = TransformLayer {
                layer: x.layer,
                transform: x.affine(),
            };
            if cmd.apply(&mut preview).is_ok() {
                let area = x.last_preview.union(&x.bbox());
                let pad = Rect::new(area.x - 2, area.y - 2, area.w + 4, area.h + 4);
                self.preview(ctx, &preview, Some(pad));
                x.last_preview = x.bbox();
            }
        }
        self.xform = Some(x);
    }

    fn handle_tool(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: impl Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        match self.tool {
            Tool::Hand => {
                if resp.hovered() {
                    ctx.set_cursor_icon(if resp.dragged() {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    });
                }
            }
            Tool::Eyedropper => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.clicked_by(primary) {
                    if let (Some(p), Some(flat)) = (resp.interact_pointer_pos(), &self.last_flat) {
                        let (x, y) = to_doc(p);
                        if x >= 0.0 && y >= 0.0 && (x as u32) < flat.width && (y as u32) < flat.height {
                            let [r, g, b, _] = flat.get(x as u32, y as u32).to_straight();
                            let enc = |v: f32| nge_io::linear_to_srgb(v) as f32 / 255.0;
                            self.brush_rgb = [enc(r), enc(g), enc(b)];
                            self.status = format!("Picked colour at {}, {}", x as i32, y as i32);
                        }
                    }
                }
            }
            Tool::Bucket => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.clicked_by(primary) {
                    if let (Some(layer), Some(p), true) =
                        (self.active, resp.interact_pointer_pos(), self.active_is_pixel())
                    {
                        let (x, y) = to_doc(p);
                        let sample = if self.sample_merged {
                            SampleSource::Merged
                        } else {
                            SampleSource::Layer(layer)
                        };
                        self.run(&BucketFill {
                            layer,
                            x: x.floor() as i32,
                            y: y.floor() as i32,
                            color: linear_rgba(self.brush_rgb, self.brush.color[3]),
                            tolerance: self.tolerance,
                            contiguous: self.contiguous,
                            sample,
                        });
                    } else if !self.active_is_pixel() {
                        self.status = "Select a pixel layer to fill".into();
                    }
                }
            }
            Tool::Wand => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        let sample = match (self.sample_merged, self.active, self.active_is_pixel()) {
                            (false, Some(layer), true) => SampleSource::Layer(layer),
                            _ => SampleSource::Merged,
                        };
                        let mods = ctx.input(|i| i.modifiers);
                        let op = if mods.shift {
                            CombineOp::Union
                        } else if mods.alt {
                            CombineOp::Subtract
                        } else {
                            self.select_op
                        };
                        self.run(&MagicWandSelect {
                            x: x.floor() as i32,
                            y: y.floor() as i32,
                            tolerance: self.tolerance,
                            contiguous: self.contiguous,
                            sample,
                            op,
                        });
                    }
                }
            }
            Tool::Gradient => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.drag_started_by(primary) {
                    if self.active_is_pixel() {
                        self.drag = Some(DragKind::Gradient);
                        self.drag_start = ctx.input(|i| i.pointer.press_origin());
                    } else {
                        self.status = "Select a pixel layer for the gradient".into();
                    }
                }
                if self.drag == Some(DragKind::Gradient) && resp.dragged_by(primary) {
                    if let (Some(a), Some(b), Some(layer)) =
                        (self.drag_start, resp.interact_pointer_pos(), self.active)
                    {
                        let cmd = self.gradient_command(layer, to_doc(a), to_doc(b));
                        let mut preview = self.editor.doc().clone();
                        if cmd.apply(&mut preview).is_ok() {
                            let area = cmd.affected(self.editor.doc());
                            self.preview(ctx, &preview, area);
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Gradient) {
                    self.drag = None;
                    let start = self.drag_start.take();
                    let end = resp
                        .interact_pointer_pos()
                        .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
                    if let (Some(a), Some(b), Some(layer)) = (start, end, self.active) {
                        if a.distance(b) >= 2.0 {
                            let cmd = self.gradient_command(layer, to_doc(a), to_doc(b));
                            self.run(&cmd);
                        } else {
                            self.mark(None);
                        }
                    }
                }
            }
            Tool::Text => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Text);
                }
                if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        let mut t =
                            TextLayer::new("Text", x, y, self.text_size, linear_rgba(self.brush_rgb, 1.0));
                        t.bold = self.text_bold;
                        let new_id = self.editor.doc().next_id();
                        self.run(&AddTextLayer {
                            text: t,
                            above: self.active,
                        });
                        self.set_active(Some(new_id));
                        self.fix_active();
                        self.status = "Text added; edit it in the options bar or Properties".into();
                    }
                }
            }
            Tool::Move => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Move);
                }
                if resp.drag_started_by(primary) {
                    if self.active_is_pixel() || self.active_is_text() {
                        self.drag = Some(DragKind::Move);
                        self.drag_start = ctx.input(|i| i.pointer.press_origin());
                        self.move_offset = (0, 0);
                    } else {
                        self.status = "Select a pixel layer to move".into();
                    }
                }
                if self.drag == Some(DragKind::Move) && resp.dragged_by(primary) {
                    if let (Some(a), Some(b)) = (self.drag_start, resp.interact_pointer_pos()) {
                        let d = (b - a) / self.zoom;
                        let off = (d.x.round() as i32, d.y.round() as i32);
                        if off != self.move_offset {
                            self.move_offset = off;
                            if let Some(layer) = self.active {
                                let mut preview = self.editor.doc().clone();
                                let cmd = MoveLayer {
                                    layer,
                                    dx: off.0,
                                    dy: off.1,
                                };
                                if cmd.apply(&mut preview).is_ok() {
                                    self.preview(ctx, &preview, None);
                                }
                            }
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Move) {
                    self.drag = None;
                    self.drag_start = None;
                    let (dx, dy) = self.move_offset;
                    if let (Some(layer), true) = (self.active, dx != 0 || dy != 0) {
                        self.run(&MoveLayer { layer, dx, dy });
                    }
                    self.mark(None);
                }
            }
            Tool::Brush | Tool::Eraser | Tool::Clone => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if self.tool == Tool::Clone {
                    let alt = ctx.input(|i| i.modifiers.alt);
                    let wants_source = self.clone_picking || self.clone_source.is_none() || alt;
                    if wants_source {
                        if resp.clicked_by(primary) || (alt && resp.drag_started_by(primary)) {
                            if let Some(p) = ctx
                                .input(|i| i.pointer.press_origin())
                                .or(resp.interact_pointer_pos())
                            {
                                self.clone_source = Some(to_doc(p));
                                self.clone_picking = false;
                                self.status = "Clone source set; now paint where the copy should go".into();
                            }
                        }
                        return;
                    }
                }
                let paintable = if self.editing_mask {
                    self.active_has_mask()
                } else {
                    self.active_is_pixel()
                };
                if resp.drag_started_by(primary) {
                    if paintable {
                        if self.tool == Tool::Clone {
                            if let (Some((sx, sy)), Some(p)) =
                                (self.clone_source, ctx.input(|i| i.pointer.press_origin()))
                            {
                                let (dx, dy) = to_doc(p);
                                self.clone_offset = ((sx - dx).round() as i32, (sy - dy).round() as i32);
                            }
                        }
                        self.drag = Some(DragKind::Stroke);
                        self.stroke.clear();
                        if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                            let (x, y) = to_doc(p);
                            self.stroke.push(StrokePoint::new(x, y, 1.0));
                        }
                    } else {
                        self.status = if self.editing_mask {
                            "The active layer has no mask to paint".into()
                        } else {
                            "Select a pixel layer to paint on".into()
                        };
                    }
                }
                if self.drag == Some(DragKind::Stroke) && resp.dragged_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let (x, y) = to_doc(p);
                        self.stroke.push(StrokePoint::new(x, y, 1.0));
                    }
                    if let (Some(layer), false) = (self.active, self.stroke.is_empty()) {
                        let cmd = self.stroke_command(layer, self.stroke.clone());
                        let mut preview = self.editor.doc().clone();
                        if cmd.apply(&mut preview).is_ok() {
                            let area = stroke_bounds(&self.make_brush(), &self.stroke, preview.canvas());
                            self.preview(ctx, &preview, Some(area));
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Stroke) {
                    self.drag = None;
                    if let Some(layer) = self.active {
                        if !self.stroke.is_empty() {
                            let pts = std::mem::take(&mut self.stroke);
                            let cmd = self.stroke_command(layer, pts);
                            self.run(cmd.as_ref());
                        }
                    }
                } else if resp.clicked_by(primary) && paintable {
                    if let (Some(layer), Some(p)) = (self.active, resp.interact_pointer_pos()) {
                        let (x, y) = to_doc(p);
                        let cmd = self.stroke_command(layer, vec![StrokePoint::new(x, y, 1.0)]);
                        self.run(cmd.as_ref());
                    }
                }
            }
            Tool::Lasso => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.drag_started_by(primary) {
                    self.drag = Some(DragKind::Lasso);
                    self.lasso.clear();
                    if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                        self.lasso.push(to_doc(p));
                    }
                }
                if self.drag == Some(DragKind::Lasso) && resp.dragged_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let q = to_doc(p);
                        let far = self
                            .lasso
                            .last()
                            .is_none_or(|l| (l.0 - q.0).abs() + (l.1 - q.1).abs() > 0.5);
                        if far {
                            self.lasso.push(q);
                        }
                    }
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Lasso) {
                    self.drag = None;
                    self.finish_polygon(ctx);
                } else if resp.clicked_by(primary) {
                    self.run(&SetSelection { selection: None });
                }
            }
            Tool::PolyLasso => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.double_clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.lasso.push(to_doc(p));
                    }
                    self.finish_polygon(ctx);
                } else if resp.clicked_by(primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let q = to_doc(p);
                        // Clicking near the first point closes the polygon.
                        if self.lasso.len() >= 3 {
                            let f = self.lasso[0];
                            if ((f.0 - q.0).powi(2) + (f.1 - q.1).powi(2)).sqrt() * self.zoom < 8.0 {
                                self.finish_polygon(ctx);
                                return;
                            }
                        }
                        self.lasso.push(q);
                    }
                }
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    self.lasso.clear();
                }
            }
            Tool::RectSelect | Tool::EllipseSelect => {
                if resp.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.drag_started_by(primary) {
                    self.drag = Some(DragKind::Select);
                    self.drag_start = ctx.input(|i| i.pointer.press_origin());
                }
                if resp.drag_stopped() && self.drag == Some(DragKind::Select) {
                    self.drag = None;
                    let press = self.drag_start.take();
                    let release = resp
                        .interact_pointer_pos()
                        .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
                    if let (Some(a), Some(b)) = (press, release) {
                        let canvas = self.editor.doc().canvas();
                        let r = drag_rect(to_doc(a), to_doc(b), canvas);
                        let mods = ctx.input(|i| i.modifiers);
                        let op = if mods.shift && mods.alt {
                            CombineOp::Intersect
                        } else if mods.shift {
                            CombineOp::Union
                        } else if mods.alt {
                            CombineOp::Subtract
                        } else {
                            self.select_op
                        };
                        if r.w < 2 || r.h < 2 {
                            self.run(&SetSelection { selection: None });
                        } else {
                            let shape = if self.tool == Tool::RectSelect {
                                Selection::rect(r)
                            } else {
                                Selection::ellipse(r)
                            };
                            self.run(&ModifySelection { shape, op });
                        }
                    }
                } else if resp.clicked_by(primary) {
                    self.run(&SetSelection { selection: None });
                }
            }
        }
    }

    fn paint_tool_overlay(&self, ctx: &egui::Context, painter: &egui::Painter, resp: &egui::Response) {
        match self.tool {
            Tool::Brush | Tool::Eraser | Tool::Clone => {
                if let (true, Some(p)) = (
                    self.drag.is_none() || self.drag == Some(DragKind::Stroke),
                    resp.hover_pos(),
                ) {
                    let r = (self.brush.radius * self.zoom).max(1.5);
                    painter.circle_stroke(p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
                    painter.circle_stroke(p, r, Stroke::new(1.0, Color32::WHITE));
                    if self.tool == Tool::Clone {
                        if let Some((sx, sy)) = self.clone_source {
                            let origin = resp.rect.min + self.pan;
                            let sp = if self.drag == Some(DragKind::Stroke) {
                                // While stroking the source follows the brush at the locked offset.
                                p + egui::vec2(self.clone_offset.0 as f32, self.clone_offset.1 as f32)
                                    * self.zoom
                            } else {
                                egui::pos2(origin.x + sx * self.zoom, origin.y + sy * self.zoom)
                            };
                            painter.circle_stroke(sp, r, Stroke::new(1.0, Color32::from_rgb(255, 170, 60)));
                            painter.line_segment(
                                [sp + egui::vec2(-6.0, 0.0), sp + egui::vec2(6.0, 0.0)],
                                Stroke::new(1.0, Color32::from_rgb(255, 170, 60)),
                            );
                            painter.line_segment(
                                [sp + egui::vec2(0.0, -6.0), sp + egui::vec2(0.0, 6.0)],
                                Stroke::new(1.0, Color32::from_rgb(255, 170, 60)),
                            );
                        }
                    }
                }
            }
            Tool::RectSelect | Tool::EllipseSelect => {
                if self.drag == Some(DragKind::Select) {
                    let press = self.drag_start;
                    let cur = ctx.input(|i| i.pointer.latest_pos());
                    if let (Some(a), Some(b)) = (press, cur) {
                        let r = egui::Rect::from_two_pos(a, b);
                        let pts: Vec<Pos2> = if self.tool == Tool::RectSelect {
                            vec![
                                r.left_top(),
                                r.right_top(),
                                r.right_bottom(),
                                r.left_bottom(),
                                r.left_top(),
                            ]
                        } else {
                            ellipse_points(r, 96)
                        };
                        painter.add(Shape::line(pts.clone(), Stroke::new(1.0, Color32::WHITE)));
                        painter.extend(Shape::dashed_line(
                            &pts,
                            Stroke::new(1.0, Color32::BLACK),
                            5.0,
                            5.0,
                        ));
                    }
                }
            }
            Tool::Lasso | Tool::PolyLasso => {
                if !self.lasso.is_empty() {
                    let origin = resp.rect.min + self.pan;
                    let zoom = self.zoom;
                    let mut pts: Vec<Pos2> = self
                        .lasso
                        .iter()
                        .map(|(x, y)| egui::pos2(origin.x + x * zoom, origin.y + y * zoom))
                        .collect();
                    if self.tool == Tool::PolyLasso {
                        if let Some(cur) = ctx.input(|i| i.pointer.latest_pos()) {
                            pts.push(cur);
                        }
                    }
                    let mut closed = pts.clone();
                    closed.push(pts[0]);
                    painter.add(Shape::line(closed.clone(), Stroke::new(1.0, Color32::WHITE)));
                    painter.extend(Shape::dashed_line(
                        &closed,
                        Stroke::new(1.0, Color32::BLACK),
                        5.0,
                        5.0,
                    ));
                    if self.tool == Tool::PolyLasso {
                        for p in &pts {
                            painter.circle_filled(*p, 3.0, Color32::WHITE);
                        }
                    }
                }
            }
            Tool::Gradient => {
                if self.drag == Some(DragKind::Gradient) {
                    if let (Some(a), Some(b)) = (self.drag_start, ctx.input(|i| i.pointer.latest_pos())) {
                        painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(140)));
                        painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
                        painter.circle_filled(a, 4.0, Color32::WHITE);
                        painter.circle_stroke(b, 4.0, Stroke::new(1.5, Color32::WHITE));
                    }
                }
            }
            Tool::Hand | Tool::Move | Tool::Eyedropper | Tool::Bucket | Tool::Wand | Tool::Text => {}
        }
    }

    // ---- dialogs ---------------------------------------------------------------------

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dialog.take() else { return };
        let title = match &d {
            Dialog::Open(_) => "Open (.nge, .psd, .png, .jpg)",
            Dialog::OpenImage(_) => "Open image",
            Dialog::PlaceImage(_) => "Place image as layer",
            Dialog::Save(_) => "Save project",
            Dialog::Export(_) => "Export PNG",
            Dialog::ExportJpeg(..) => "Export JPEG",
            Dialog::ExportPsd(_) => "Export Photoshop PSD",
            Dialog::New(..) => "New document",
            Dialog::Filter(f) => f.name(),
            Dialog::CanvasSize(..) => "Canvas size",
            Dialog::ImageSize(..) => "Image size",
        };
        let mut keep = true;
        let mut confirmed = false;
        let mut filter_changed = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                match &mut d {
                    Dialog::Open(p)
                    | Dialog::OpenImage(p)
                    | Dialog::PlaceImage(p)
                    | Dialog::Save(p)
                    | Dialog::Export(p)
                    | Dialog::ExportPsd(p) => {
                        ui.label("File path:");
                        let r = ui.add(egui::TextEdit::singleline(p).desired_width(380.0));
                        r.request_focus();
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            confirmed = true;
                        }
                    }
                    Dialog::ExportJpeg(p, q) => {
                        ui.label("File path:");
                        ui.add(egui::TextEdit::singleline(p).desired_width(380.0));
                        let mut qf = *q as f32;
                        ui.add(egui::Slider::new(&mut qf, 1.0..=100.0).integer().text("Quality"));
                        *q = qf.round() as u8;
                        ui.label(RichText::new("Transparent areas are flattened onto white.").weak());
                    }
                    Dialog::New(w, h) => {
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            ui.add(egui::DragValue::new(w).range(1..=16384).suffix(" px"));
                            ui.label("Height");
                            ui.add(egui::DragValue::new(h).range(1..=16384).suffix(" px"));
                        });
                    }
                    Dialog::Filter(f) => match f {
                        Filter::GaussianBlur { radius } | Filter::BoxBlur { radius } => {
                            filter_changed |= ui
                                .add(
                                    egui::Slider::new(radius, 0.5..=60.0)
                                        .logarithmic(true)
                                        .suffix(" px")
                                        .text("Radius"),
                                )
                                .changed();
                        }
                        Filter::Sharpen { amount, radius } => {
                            filter_changed |= ui
                                .add(egui::Slider::new(amount, 0.0..=5.0).text("Amount"))
                                .changed();
                            filter_changed |= ui
                                .add(egui::Slider::new(radius, 0.5..=20.0).suffix(" px").text("Radius"))
                                .changed();
                        }
                    },
                    Dialog::CanvasSize(w, h, anchor) => {
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            ui.add(egui::DragValue::new(w).range(1..=16384).suffix(" px"));
                            ui.label("Height");
                            ui.add(egui::DragValue::new(h).range(1..=16384).suffix(" px"));
                        });
                        ui.label("Anchor");
                        for row in 0..3 {
                            ui.horizontal(|ui| {
                                for col in 0..3 {
                                    let a = (col as f32 * 0.5, row as f32 * 0.5);
                                    let on = (anchor.0 - a.0).abs() < 1e-3 && (anchor.1 - a.1).abs() < 1e-3;
                                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                                    let fill = if on { ACCENT } else { Color32::from_gray(60) };
                                    ui.painter().rect_filled(r.shrink(3.0), 3.0, fill);
                                    if on {
                                        ui.painter().circle_filled(r.center(), 4.0, Color32::WHITE);
                                    }
                                    if resp.clicked() {
                                        *anchor = a;
                                    }
                                }
                            });
                        }
                    }
                    Dialog::ImageSize(w, h, lock) => {
                        let (ow, oh) = (self.editor.doc().width as f32, self.editor.doc().height as f32);
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            let rw = ui.add(egui::DragValue::new(w).range(1..=16384).suffix(" px"));
                            ui.label("Height");
                            let rh = ui.add(egui::DragValue::new(h).range(1..=16384).suffix(" px"));
                            if *lock {
                                if rw.changed() {
                                    *h = ((*w as f32) * oh / ow).round().max(1.0) as u32;
                                } else if rh.changed() {
                                    *w = ((*h as f32) * ow / oh).round().max(1.0) as u32;
                                }
                            }
                        });
                        ui.checkbox(lock, "Keep aspect ratio");
                        ui.label(RichText::new("Resamples every layer bilinearly.").weak());
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });

        if let Dialog::Filter(f) = &d {
            if filter_changed || !self.filter_previewed {
                if let Some(layer) = self.active {
                    let mut preview = self.editor.doc().clone();
                    if (ApplyFilter {
                        layer,
                        filter: f.clone(),
                    })
                    .apply(&mut preview)
                    .is_ok()
                    {
                        self.preview(ctx, &preview, None);
                    }
                }
                self.filter_previewed = true;
            }
        }

        if confirmed {
            match &d {
                Dialog::Open(p) => self.open_path(p),
                Dialog::OpenImage(p) => self.open_image(p),
                Dialog::PlaceImage(p) => self.place_image(p),
                Dialog::Save(p) => self.save_path(p),
                Dialog::Export(p) => self.export_png(p),
                Dialog::ExportJpeg(p, q) => self.export_jpeg(p, *q),
                Dialog::ExportPsd(p) => self.export_psd(p),
                Dialog::New(w, h) => self.set_doc(blank(*w, *h), None),
                Dialog::Filter(f) => {
                    if let Some(layer) = self.active {
                        self.run(&ApplyFilter {
                            layer,
                            filter: f.clone(),
                        });
                    }
                }
                Dialog::CanvasSize(w, h, anchor) => {
                    self.run(&ResizeCanvas {
                        width: *w,
                        height: *h,
                        anchor: *anchor,
                    });
                    self.view_cmd = Some(ViewCmd::Fit);
                }
                Dialog::ImageSize(w, h, _) => {
                    self.run(&ResizeImage {
                        width: *w,
                        height: *h,
                    });
                    self.view_cmd = Some(ViewCmd::Fit);
                }
            }
            keep = false;
        }
        if keep {
            self.dialog = Some(d);
        } else {
            if matches!(d, Dialog::Filter(_)) {
                // Drop the preview whether confirmed or cancelled.
                self.mark(None);
            }
            self.filter_previewed = false;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.shortcuts(ctx);
        self.menu_bar(ctx);
        self.options_bar(ctx);
        self.status_bar(ctx);
        self.tool_palette(ctx);
        self.side_panel(ctx);
        self.canvas(ctx);
        self.dialogs(ctx);
        if self.dirty {
            self.refresh(ctx);
            ctx.request_repaint();
        }
    }
}

// ---- free helpers --------------------------------------------------------------------

fn blank(w: u32, h: u32) -> Editor {
    let mut ed = Editor::new(Document::new(w, h));
    let paper = Raster::filled(w, h, nge_tiles::Rgba::WHITE);
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
    ]
}

fn nearest_when_zoomed() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..Default::default()
    }
}

fn upload(
    slot: &mut Option<TextureHandle>,
    ctx: &egui::Context,
    name: &str,
    img: egui::ColorImage,
    opts: egui::TextureOptions,
) {
    match slot {
        Some(t) => t.set(img, opts),
        None => *slot = Some(ctx.load_texture(name, img, opts)),
    }
}

fn srgb_lut() -> &'static [u8; 4096] {
    static LUT: OnceLock<[u8; 4096]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 4096];
        for (i, v) in t.iter_mut().enumerate() {
            *v = nge_io::linear_to_srgb(i as f32 / 4095.0);
        }
        t
    })
}

fn to_color32(p: nge_tiles::Rgba) -> Color32 {
    let lut = srgb_lut();
    let enc = |v: f32| lut[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize];
    let [r, g, b, a] = p.to_straight();
    Color32::from_rgba_unmultiplied(enc(r), enc(g), enc(b), (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

fn composite_image(doc: &Document) -> egui::ColorImage {
    raster_to_image(&nge_render::composite_raster(doc))
}

fn raster_to_image(flat: &Raster) -> egui::ColorImage {
    egui::ColorImage {
        size: [flat.width as usize, flat.height as usize],
        pixels: flat.pixels.iter().map(|p| to_color32(*p)).collect(),
    }
}

/// Sample a layer onto a small thumbnail (nearest, centre of each cell).
fn thumb_image(canvas: Rect, sample: impl Fn(i32, i32) -> nge_tiles::Rgba) -> egui::ColorImage {
    let (tw, th) = THUMB;
    let scale = (canvas.w as f32 / tw as f32)
        .max(canvas.h as f32 / th as f32)
        .max(1e-3);
    let (cw, ch) = (canvas.w as f32 / scale, canvas.h as f32 / scale);
    let (ox, oy) = ((tw as f32 - cw) / 2.0, (th as f32 - ch) / 2.0);
    let mut pixels = vec![Color32::TRANSPARENT; tw * th];
    for ty in 0..th {
        for tx in 0..tw {
            let fx = tx as f32 + 0.5 - ox;
            let fy = ty as f32 + 0.5 - oy;
            if fx < 0.0 || fy < 0.0 || fx >= cw || fy >= ch {
                continue;
            }
            let x = canvas.x + (fx * scale) as i32;
            let y = canvas.y + (fy * scale) as i32;
            pixels[ty * tw + tx] = to_color32(sample(x, y));
        }
    }
    egui::ColorImage {
        size: [tw, th],
        pixels,
    }
}

fn paint_thumb_bg(p: &egui::Painter, r: egui::Rect) {
    p.rect_filled(r, 2.0, Color32::from_gray(200));
    let cell = 5.0;
    let mut y = r.min.y;
    let mut row = 0;
    while y < r.max.y {
        let mut x = r.min.x + if row % 2 == 0 { 0.0 } else { cell };
        while x < r.max.x {
            let c = egui::Rect::from_min_size(egui::pos2(x, y), Vec2::splat(cell)).intersect(r);
            p.rect_filled(c, 0.0, Color32::from_gray(150));
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

/// Selection outline overlay: pixels on the 50% coverage boundary alternate
/// black and white.
fn selection_overlay(doc: &Document) -> Option<egui::ColorImage> {
    let sel = doc.selection.as_ref()?;
    let (w, h) = (doc.width as i32, doc.height as i32);
    let area = sel.bounds_within(doc.canvas());
    if area.is_empty() {
        return None;
    }
    let (x0, y0) = ((area.x - 1).max(0), (area.y - 1).max(0));
    let (x1, y1) = ((area.right() + 1).min(w), (area.bottom() + 1).min(h));
    let (gw, gh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut inside = vec![false; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            inside[gy * gw + gx] = sel.value(x0 + gx as i32, y0 + gy as i32) >= 0.5;
        }
    }
    let at = |gx: i32, gy: i32| {
        gx >= 0
            && gy >= 0
            && (gx as usize) < gw
            && (gy as usize) < gh
            && inside[gy as usize * gw + gx as usize]
    };
    let mut pixels = vec![Color32::TRANSPARENT; (w * h) as usize];
    for gy in 0..gh as i32 {
        for gx in 0..gw as i32 {
            if at(gx, gy) && !(at(gx - 1, gy) && at(gx + 1, gy) && at(gx, gy - 1) && at(gx, gy + 1)) {
                let (x, y) = (x0 + gx, y0 + gy);
                pixels[(y * w + x) as usize] = if ((x + y) / 4) % 2 == 0 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                };
            }
        }
    }
    Some(egui::ColorImage {
        size: [w as usize, h as usize],
        pixels,
    })
}

fn drag_rect(a: (f32, f32), b: (f32, f32), canvas: Rect) -> Rect {
    let (x0, x1) = (a.0.min(b.0).round() as i32, a.0.max(b.0).round() as i32);
    let (y0, y1) = (a.1.min(b.1).round() as i32, a.1.max(b.1).round() as i32);
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&canvas)
}

fn ellipse_points(r: egui::Rect, n: usize) -> Vec<Pos2> {
    let c = r.center();
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            egui::pos2(c.x + t.cos() * r.width() / 2.0, c.y + t.sin() * r.height() / 2.0)
        })
        .collect()
}

fn point_in_convex(poly: &[Pos2], q: Pos2) -> bool {
    let n = poly.len();
    let mut sign = 0.0f32;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let cross = (b.x - a.x) * (q.y - a.y) - (b.y - a.y) * (q.x - a.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

fn paint_xform_box(painter: &egui::Painter, x: &Xform, to_screen: impl Fn(f32, f32) -> Pos2) {
    let pts: Vec<Pos2> = x.corners().iter().map(|(px, py)| to_screen(*px, *py)).collect();
    let mut closed = pts.clone();
    closed.push(pts[0]);
    painter.add(Shape::line(
        closed.clone(),
        Stroke::new(2.0, Color32::from_black_alpha(140)),
    ));
    painter.add(Shape::line(closed, Stroke::new(1.0, Color32::WHITE)));
    for p in &pts {
        painter.rect_filled(
            egui::Rect::from_center_size(*p, Vec2::splat(8.0)),
            1.0,
            Color32::WHITE,
        );
        painter.rect_stroke(
            egui::Rect::from_center_size(*p, Vec2::splat(8.0)),
            1.0,
            Stroke::new(1.0, ACCENT),
        );
    }
    let (cx, cy) = x.center();
    let c = to_screen(cx + x.dx, cy + x.dy);
    painter.circle_stroke(c, 5.0, Stroke::new(1.0, Color32::WHITE));
    painter.line_segment(
        [c + egui::vec2(-8.0, 0.0), c + egui::vec2(8.0, 0.0)],
        Stroke::new(1.0, Color32::WHITE),
    );
    painter.line_segment(
        [c + egui::vec2(0.0, -8.0), c + egui::vec2(0.0, 8.0)],
        Stroke::new(1.0, Color32::WHITE),
    );
}

fn paint_checker(painter: &egui::Painter, doc_rect: egui::Rect, clip: egui::Rect) {
    let visible = doc_rect.intersect(clip);
    if visible.width() <= 0.0 || visible.height() <= 0.0 {
        return;
    }
    painter.rect_filled(visible, 0.0, Color32::from_gray(222));
    let cell = 12.0;
    let (i0, i1) = (
        ((visible.min.x - doc_rect.min.x) / cell).floor() as i32,
        ((visible.max.x - doc_rect.min.x) / cell).ceil() as i32,
    );
    let (j0, j1) = (
        ((visible.min.y - doc_rect.min.y) / cell).floor() as i32,
        ((visible.max.y - doc_rect.min.y) / cell).ceil() as i32,
    );
    for j in j0..j1 {
        for i in i0..i1 {
            if (i + j) % 2 != 0 {
                let r = egui::Rect::from_min_size(
                    doc_rect.min + egui::vec2(i as f32 * cell, j as f32 * cell),
                    Vec2::splat(cell),
                )
                .intersect(visible);
                painter.rect_filled(r, 0.0, Color32::from_gray(188));
            }
        }
    }
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(text)
            .small()
            .strong()
            .color(Color32::from_gray(150)),
    );
}

fn badge(p: &egui::Painter, rect: egui::Rect, kind: Kind) {
    let (col, txt) = match kind {
        Kind::Pixel => (Color32::from_rgb(70, 130, 200), "P"),
        Kind::Adjustment => (Color32::from_rgb(205, 140, 45), "A"),
        Kind::Group => (Color32::from_rgb(90, 170, 110), "G"),
        Kind::Filter => (Color32::from_rgb(150, 90, 190), "F"),
        Kind::Text => (Color32::from_rgb(60, 150, 150), "T"),
    };
    p.rect_filled(rect, 2.0, col);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        txt,
        FontId::proportional(13.0),
        Color32::WHITE,
    );
}

/// Returns true when the user finished an edit (drag released or value typed).
fn slider_row(ui: &mut egui::Ui, label: &str, v: &mut f32, range: RangeInclusive<f32>, suffix: &str) -> bool {
    let r = ui.add(
        egui::Slider::new(v, range)
            .text(label)
            .suffix(suffix)
            .fixed_decimals(2),
    );
    r.drag_stopped() || (r.changed() && !r.dragged())
}

/// An interactive curve: drag points, click to add, right-click to remove.
fn curve_editor(ui: &mut egui::Ui, points: &mut Vec<[f32; 2]>, drag: &mut Option<usize>) -> bool {
    let size = Vec2::splat(210.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, Color32::from_gray(22));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let gx = rect.min.x + t * rect.width();
        let gy = rect.min.y + t * rect.height();
        let grid = Stroke::new(1.0, Color32::from_gray(44));
        p.line_segment([egui::pos2(gx, rect.min.y), egui::pos2(gx, rect.max.y)], grid);
        p.line_segment([egui::pos2(rect.min.x, gy), egui::pos2(rect.max.x, gy)], grid);
    }
    let to_screen =
        |x: f32, y: f32| egui::pos2(rect.min.x + x * rect.width(), rect.max.y - y * rect.height());
    let from_screen = |q: Pos2| {
        (
            ((q.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
            ((rect.max.y - q.y) / rect.height()).clamp(0.0, 1.0),
        )
    };
    p.line_segment(
        [to_screen(0.0, 0.0), to_screen(1.0, 1.0)],
        Stroke::new(1.0, Color32::from_gray(70)),
    );
    let nearest = |pts: &[[f32; 2]], q: Pos2| -> Option<usize> {
        pts.iter()
            .enumerate()
            .map(|(i, pt)| (i, to_screen(pt[0], pt[1]).distance(q)))
            .filter(|(_, d)| *d <= 14.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut finished = false;
    if resp.drag_started() {
        if let Some(q) = ui.input(|i| i.pointer.press_origin()) {
            *drag = nearest(points, q);
        }
    }
    if resp.dragged() {
        if let (Some(i), Some(q)) = (*drag, resp.interact_pointer_pos()) {
            let (x, y) = from_screen(q);
            let last = points.len() - 1;
            let x = if i == 0 || i == last {
                points[i][0]
            } else {
                x.clamp(points[i - 1][0] + 0.01, points[i + 1][0] - 0.01)
            };
            points[i] = [x, y];
        }
    }
    if resp.drag_stopped() {
        *drag = None;
        finished = true;
    }
    if resp.clicked() {
        if let Some(q) = resp.interact_pointer_pos() {
            if nearest(points, q).is_none() && points.len() < 16 {
                let (x, y) = from_screen(q);
                let at = points.iter().position(|pt| pt[0] > x).unwrap_or(points.len());
                if at > 0 && at < points.len() {
                    points.insert(at, [x, y]);
                }
                finished = true;
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(q) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(points, q) {
                if i != 0 && i != points.len() - 1 && points.len() > 2 {
                    points.remove(i);
                    finished = true;
                }
            }
        }
    }
    let compiled = Adjustment::Curves {
        points: points.clone(),
    }
    .compile();
    let line: Vec<Pos2> = (0..=64)
        .map(|i| {
            let x = i as f32 / 64.0;
            to_screen(x, compiled.apply([x; 3])[0])
        })
        .collect();
    p.add(Shape::line(
        line,
        Stroke::new(2.0, Color32::from_rgb(140, 185, 255)),
    ));
    for pt in points.iter() {
        let c = to_screen(pt[0], pt[1]);
        p.circle_filled(c, 5.0, Color32::WHITE);
        p.circle_stroke(c, 5.0, Stroke::new(1.5, ACCENT));
    }
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(70)));
    ui.horizontal(|ui| {
        let mut preset = None;
        if ui.small_button("Linear").clicked() {
            preset = Some(vec![[0.0, 0.0], [1.0, 1.0]]);
        }
        if ui.small_button("Contrast").clicked() {
            preset = Some(vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]);
        }
        if ui.small_button("Lighten").clicked() {
            preset = Some(vec![[0.0, 0.0], [0.5, 0.65], [1.0, 1.0]]);
        }
        if ui.small_button("Fade").clicked() {
            preset = Some(vec![[0.0, 0.12], [1.0, 0.9]]);
        }
        if let Some(pts) = preset {
            *points = pts;
            finished = true;
        }
    });
    finished
}

fn draw_icon(p: &egui::Painter, r: egui::Rect, tool: Tool) {
    let c = Color32::WHITE;
    let s = Stroke::new(1.6, c);
    match tool {
        Tool::Brush => {
            p.line_segment(
                [
                    egui::pos2(r.min.x + 1.0, r.max.y - 1.0),
                    egui::pos2(r.center().x + 2.0, r.center().y - 2.0),
                ],
                Stroke::new(3.0, c),
            );
            p.circle_filled(egui::pos2(r.max.x - 4.0, r.min.y + 4.0), 4.5, c);
        }
        Tool::RectSelect => {
            let pts = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            p.extend(Shape::dashed_line(&pts, s, 3.0, 2.5));
        }
        Tool::EllipseSelect => {
            p.extend(Shape::dashed_line(&ellipse_points(r, 40), s, 3.0, 2.5));
        }
        Tool::Hand => {
            let ctr = r.center();
            let h = r.width() / 2.0;
            for v in [
                egui::vec2(0.0, -h),
                egui::vec2(0.0, h),
                egui::vec2(-h, 0.0),
                egui::vec2(h, 0.0),
            ] {
                p.arrow(ctr, v, s);
            }
        }
        Tool::Move => {
            let pts = vec![
                egui::pos2(r.min.x + 2.0, r.min.y),
                egui::pos2(r.min.x + 2.0, r.max.y - 3.0),
                egui::pos2(r.min.x + 7.0, r.max.y - 8.0),
                egui::pos2(r.min.x + 10.0, r.max.y),
                egui::pos2(r.min.x + 13.0, r.max.y - 2.0),
                egui::pos2(r.min.x + 10.0, r.max.y - 9.0),
                egui::pos2(r.max.x, r.max.y - 9.0),
            ];
            p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
        }
        Tool::Eraser => {
            let ctr = r.center();
            let a = egui::pos2(ctr.x - 6.0, ctr.y + 6.0);
            let b = egui::pos2(ctr.x + 6.0, ctr.y - 6.0);
            p.line_segment([a, b], Stroke::new(7.0, c));
            p.line_segment(
                [
                    egui::pos2(ctr.x - 8.0, ctr.y + 10.0),
                    egui::pos2(ctr.x + 1.0, ctr.y + 10.0),
                ],
                s,
            );
        }
        Tool::Eyedropper => {
            let ctr = r.center();
            p.line_segment(
                [
                    egui::pos2(ctr.x - 7.0, ctr.y + 7.0),
                    egui::pos2(ctr.x + 3.0, ctr.y - 3.0),
                ],
                Stroke::new(2.5, c),
            );
            p.circle_filled(egui::pos2(ctr.x + 5.0, ctr.y - 5.0), 4.0, c);
        }
        Tool::Clone => {
            let ctr = r.center();
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(ctr.x, ctr.y - 6.0), egui::vec2(6.0, 8.0)),
                2.0,
                c,
            );
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(ctr.x, ctr.y + 1.0), egui::vec2(3.0, 6.0)),
                0.0,
                c,
            );
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(ctr.x, ctr.y + 7.0), egui::vec2(18.0, 6.0)),
                2.0,
                c,
            );
        }
        Tool::Text => {
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                "T",
                FontId::proportional(20.0),
                c,
            );
        }
        Tool::Bucket => {
            let ctr = r.center();
            let pts = vec![
                egui::pos2(ctr.x - 8.0, ctr.y - 2.0),
                egui::pos2(ctr.x + 1.0, ctr.y - 9.0),
                egui::pos2(ctr.x + 8.0, ctr.y - 1.0),
                egui::pos2(ctr.x - 1.0, ctr.y + 7.0),
            ];
            p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
            p.circle_filled(egui::pos2(ctr.x + 7.0, ctr.y + 7.0), 2.5, c);
        }
        Tool::Gradient => {
            let steps = 6;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let x0 = r.min.x + r.width() * i as f32 / steps as f32;
                let x1 = r.min.x + r.width() * (i + 1) as f32 / steps as f32;
                let g = (255.0 * (1.0 - t * 0.8)) as u8;
                p.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x0, r.min.y + 2.0), egui::pos2(x1, r.max.y - 2.0)),
                    0.0,
                    Color32::from_gray(g),
                );
            }
        }
        Tool::Lasso => {
            let ctr = r.center();
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0 * std::f32::consts::TAU;
                    egui::pos2(
                        ctr.x + t.cos() * 9.0 + (t * 2.0).sin() * 2.0,
                        ctr.y - 2.0 + t.sin() * 6.0,
                    )
                })
                .collect();
            p.extend(Shape::dashed_line(&pts, s, 3.0, 2.0));
            p.line_segment(
                [
                    egui::pos2(ctr.x + 7.0, ctr.y + 3.0),
                    egui::pos2(ctr.x + 4.0, ctr.y + 10.0),
                ],
                s,
            );
        }
        Tool::PolyLasso => {
            let pts = [
                egui::pos2(r.min.x, r.min.y + 4.0),
                egui::pos2(r.center().x + 2.0, r.min.y),
                egui::pos2(r.max.x, r.center().y),
                egui::pos2(r.max.x - 6.0, r.max.y),
                egui::pos2(r.min.x + 3.0, r.max.y - 4.0),
                egui::pos2(r.min.x, r.min.y + 4.0),
            ];
            p.extend(Shape::dashed_line(&pts, s, 3.0, 2.0));
        }
        Tool::Wand => {
            let ctr = r.center();
            p.line_segment(
                [
                    egui::pos2(ctr.x - 7.0, ctr.y + 7.0),
                    egui::pos2(ctr.x + 2.0, ctr.y - 2.0),
                ],
                Stroke::new(2.5, c),
            );
            for (dx, dy) in [(6.0, -6.0), (9.0, -2.0), (3.0, -9.0)] {
                p.circle_filled(egui::pos2(ctr.x + dx, ctr.y + dy), 1.8, c);
            }
        }
    }
}
