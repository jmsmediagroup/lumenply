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

mod canvas;
mod dialogs;
mod history;
mod layers;
mod menu;
mod options_bar;
mod properties;
mod status;
mod theme;
mod tools;

pub(crate) use canvas::*;
pub(crate) use dialogs::Dialog;
pub(crate) use theme::*;
pub(crate) use tools::Tool;

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
        theme::install(&cc.egui_ctx);

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
        self.drag_start = None;
        self.stroke.clear();
        self.lasso.clear();
        self.clone_source = None;
        self.clone_picking = true;
        self.clone_offset = (0, 0);
        self.move_offset = (0, 0);
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
        self.cancel_interaction();
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

    // ---- shortcuts --------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Modifiers as M;
        if ctx.wants_keyboard_input() {
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
