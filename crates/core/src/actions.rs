//! Actions: Photoshop's recorded macros. An [`Action`] is a named list of
//! [`Step`]s at the level a user thinks in ("add a Curves layer with these
//! points", "Image Size 2048 × 1365", "Flatten"), not raw commands: commands
//! hold layer ids that mean nothing in another document, steps act on the
//! *active layer at replay time*. Steps serialise as JSON, so actions can be
//! saved, shared and replayed by the app or the headless batch (ADR 0023).
//!
//! Playback goes through an [`ActionHost`]: the app runs registry actions
//! (`Step::Menu`) with its own UI context; [`CoreHost`] runs the ones the
//! engine can do alone, for the CLI and tests.

use lumenply_doc::{Adjustment, Document, Filter, LayerId, Selection};
use serde::{Deserialize, Serialize};

use crate::canvas_ops::{RevealAll, RotateCanvas, Trim, TrimBasis};
use crate::commands::{
    AddAdjustmentLayer, AddPixelLayer, ApplyFilter, CropDocument, FlipImage, InvertSelection, RemoveLayer,
    ResizeCanvas, ResizeImage, RotateImage, SetSelection,
};
use crate::layer_ops::{DuplicateLayer, FlattenImage, MergeVisible, StampVisible};
use crate::smart_filter_cmds::AddSmartFilter;
use crate::{Command, Editor};

/// One recorded edit. Steps never name layers: they act on whatever layer
/// is active when the action plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum Step {
    /// A parameter-free menu action from the app's registry, by id
    /// (e.g. `flatten`, `invert-sel`, `rot-cw`); see [`RECORDABLE_MENU`].
    Menu { id: String },
    /// Add an adjustment layer above the active layer, with these settings.
    Adjust { adjustment: Adjustment },
    /// Apply an adjustment to the active layer's pixels (Image ▸
    /// Adjustments, "apply to pixels").
    Apply { adjustment: Adjustment },
    /// Run a filter on the active layer: baked into a pixel layer, or added
    /// as a smart filter on a smart object (as the filter dialogs do).
    Filter { filter: Filter },
    /// Image ▸ Image Size to exactly this many pixels, and to this
    /// resolution (ppi) when one was set; same pixels = no resampling.
    ImageSize {
        width: u32,
        height: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<f32>,
    },
    /// Scale (never enlarge) so the longer side is at most `long_edge`
    /// pixels, keeping the aspect ratio: Photoshop's Fit Image, for actions
    /// that must work on images of any size.
    FitImage { long_edge: u32 },
    /// Image ▸ Canvas Size; `anchor` is where the old canvas sits (0..1).
    CanvasSize {
        width: u32,
        height: u32,
        anchor: [f32; 2],
    },
    /// Image ▸ Rotate by angle, clockwise degrees.
    RotateCanvas { degrees: f32 },
    /// Image ▸ Trim: transparent borders, else ones the top-left colour.
    Trim { transparent: bool },
    /// An edit made while recording that can't be recorded yet (a brush
    /// stroke, a drag). Kept so the panel can say so; playback skips it.
    Skipped { what: String },
}

/// Registry ids a [`Step::Menu`] may name: parameter-free edits of the
/// document, with their labels. Anything else (file dialogs, view toggles,
/// tools) is refused at playback so a shared action can't open dialogs.
pub const RECORDABLE_MENU: &[(&str, &str)] = &[
    ("select-all", "Select all"),
    ("deselect", "Deselect"),
    ("invert-sel", "Invert selection"),
    ("new-layer", "New layer"),
    ("duplicate-layer", "Duplicate layer"),
    ("delete-layer", "Delete layer"),
    ("layer-via-copy", "Layer via copy"),
    ("merge-down", "Merge down"),
    ("merge-visible", "Merge visible"),
    ("flatten", "Flatten image"),
    ("stamp-visible", "Stamp visible"),
    ("group", "Group layers"),
    ("ungroup", "Ungroup"),
    ("layer-up", "Move layer up"),
    ("layer-down", "Move layer down"),
    ("add-mask", "Add layer mask"),
    ("rm-mask", "Remove layer mask"),
    ("mask-from-sel", "Layer mask from selection"),
    ("clip", "Clip to layer below"),
    ("unclip", "Release clip"),
    ("smart-object", "Convert to smart object"),
    ("rasterize", "Rasterize layer"),
    ("flip-h", "Flip layer horizontal"),
    ("flip-v", "Flip layer vertical"),
    ("rot-cw", "Rotate image 90° clockwise"),
    ("rot-ccw", "Rotate image 90° counter-clockwise"),
    ("rot-180", "Rotate image 180°"),
    ("img-flip-h", "Flip image horizontal"),
    ("img-flip-v", "Flip image vertical"),
    ("crop", "Crop to selection"),
    ("reveal-all", "Reveal all"),
    ("auto-contrast", "Auto contrast"),
    ("auto-color", "Auto color"),
    ("auto-tone", "Auto tone"),
    ("adj-desaturate", "Desaturate"),
    ("adjd-invert", "Invert"),
    ("fill", "Fill with foreground colour"),
    ("clear", "Clear"),
];

/// Is `id` a registry action an action may record and replay?
pub fn menu_recordable(id: &str) -> bool {
    RECORDABLE_MENU.iter().any(|(i, _)| *i == id)
}

/// The label of a recordable registry id (the id itself when unknown).
pub fn menu_label(id: &str) -> &str {
    RECORDABLE_MENU
        .iter()
        .find(|(i, _)| *i == id)
        .map_or(id, |(_, l)| l)
}

impl Step {
    /// What the Actions panel lists for this step.
    pub fn describe(&self) -> String {
        match self {
            Step::Menu { id } => menu_label(id).to_string(),
            Step::Adjust { adjustment } => format!("Add {} layer", adjustment.name()),
            Step::Apply { adjustment } => format!("{} (pixels)", adjustment.name()),
            Step::Filter { filter } => filter.name().to_string(),
            Step::ImageSize {
                width,
                height,
                resolution: None,
            } => format!("Image size {width} × {height}"),
            Step::ImageSize {
                width,
                height,
                resolution: Some(ppi),
            } => format!("Image size {width} × {height} at {ppi} ppi"),
            Step::FitImage { long_edge } => format!("Fit image: long edge {long_edge} px"),
            Step::CanvasSize { width, height, .. } => format!("Canvas size {width} × {height}"),
            Step::RotateCanvas { degrees } => format!("Rotate canvas {degrees}°"),
            Step::Trim { transparent: true } => "Trim transparent borders".into(),
            Step::Trim { transparent: false } => "Trim top-left colour borders".into(),
            Step::Skipped { what } => format!("not recordable yet: {what}"),
        }
    }
}

/// A named, replayable list of steps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Shipped with Lumenply (not saved with the user's actions; can't be
    /// renamed or deleted).
    #[serde(skip)]
    pub builtin: bool,
}

impl Action {
    pub fn new(name: impl Into<String>) -> Self {
        Action {
            name: name.into(),
            steps: Vec::new(),
            builtin: false,
        }
    }

    /// Steps playback will run (everything but [`Step::Skipped`] notes).
    pub fn playable(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| !matches!(s, Step::Skipped { .. }))
            .count()
    }
}

/// The on-disk form: `{"version": 1, "actions": [...]}`.
#[derive(Serialize)]
struct ActionFile<'a> {
    version: u32,
    actions: Vec<&'a Action>,
}

/// JSON for a set of actions (built-ins are left out).
pub fn to_json(actions: &[Action]) -> String {
    let file = ActionFile {
        version: 1,
        actions: actions.iter().filter(|a| !a.builtin).collect(),
    };
    serde_json::to_string_pretty(&file).expect("actions serialise")
}

/// Read actions from JSON: a set (`{"actions": [...]}`), a list, or a
/// single action. Unknown step kinds and bad fields are errors naming the
/// action, never panics.
pub fn from_json(text: &str) -> Result<Vec<Action>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let list = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(mut o) => match o.remove("actions") {
            Some(serde_json::Value::Array(a)) => a,
            Some(_) => return Err("\"actions\" must be a list".into()),
            None if o.contains_key("steps") => vec![serde_json::Value::Object(o)],
            None => return Err("no actions: expected {\"actions\": [...]} or one action".into()),
        },
        _ => return Err("expected an action, a list of actions or {\"actions\": [...]}".into()),
    };
    list.into_iter()
        .enumerate()
        .map(|(i, a)| {
            let name = a
                .get("name")
                .and_then(|n| n.as_str())
                .map_or_else(|| format!("#{}", i + 1), |n| format!("\"{n}\""));
            serde_json::from_value::<Action>(a).map_err(|e| format!("action {name}: {e}"))
        })
        .collect()
}

/// What playback needs from its front end.
pub trait ActionHost {
    fn editor(&mut self) -> &mut Editor;
    /// The layer steps act on, if any.
    fn active_layer(&self) -> Option<LayerId>;
    fn set_active_layer(&mut self, id: Option<LayerId>);
    /// Run a recordable registry action (see [`RECORDABLE_MENU`]). The
    /// default runs the ones the engine can do on its own.
    fn run_menu(&mut self, id: &str) -> Result<(), String> {
        run_core_menu(self, id)
    }
}

/// A host with no UI: an editor and an active layer (the top one to start).
pub struct CoreHost<'a> {
    pub editor: &'a mut Editor,
    pub active: Option<LayerId>,
}

impl<'a> CoreHost<'a> {
    pub fn new(editor: &'a mut Editor) -> Self {
        let active = editor.doc().layers().last().map(|l| l.id);
        CoreHost { editor, active }
    }
}

impl ActionHost for CoreHost<'_> {
    fn editor(&mut self) -> &mut Editor {
        self.editor
    }
    fn active_layer(&self) -> Option<LayerId> {
        self.active.filter(|id| self.editor.doc().layer(*id).is_some())
    }
    fn set_active_layer(&mut self, id: Option<LayerId>) {
        self.active = id;
    }
}

fn exec<H: ActionHost + ?Sized>(host: &mut H, cmd: &dyn Command) -> Result<(), String> {
    host.editor().execute(cmd).map_err(|e| e.to_string())
}

fn top_layer(doc: &Document) -> Option<LayerId> {
    doc.layers().last().map(|l| l.id)
}

/// Run a command that creates one layer (its id is `next_id()` before it
/// runs) and make that layer active.
fn exec_new_layer<H: ActionHost + ?Sized>(host: &mut H, cmd: &dyn Command) -> Result<(), String> {
    let id = host.editor().doc().next_id();
    exec(host, cmd)?;
    if host.editor().doc().layer(id).is_some() {
        host.set_active_layer(Some(id));
    }
    Ok(())
}

/// The registry actions the engine runs without the app. Others (auto
/// colour needs the composite histogram, masks and groups the app's layer
/// logic) fail with a message saying they need the app.
pub fn run_core_menu<H: ActionHost + ?Sized>(host: &mut H, id: &str) -> Result<(), String> {
    let need_layer = || "this step needs an active layer".to_string();
    match id {
        "select-all" => exec(
            host,
            &SetSelection {
                selection: Some(Selection::all()),
            },
        ),
        "deselect" => exec(host, &SetSelection { selection: None }),
        "invert-sel" => exec(host, &InvertSelection),
        "new-layer" => {
            let n = host.editor().doc().layer_count() + 1;
            exec_new_layer(host, &AddPixelLayer::new(format!("Layer {n}")))
        }
        "duplicate-layer" => {
            let layer = host.active_layer().ok_or_else(need_layer)?;
            exec_new_layer(host, &DuplicateLayer { layer })
        }
        "delete-layer" => {
            let layer = host.active_layer().ok_or_else(need_layer)?;
            exec(host, &RemoveLayer { layer })?;
            let top = top_layer(host.editor().doc());
            host.set_active_layer(top);
            Ok(())
        }
        "merge-visible" => {
            exec(host, &MergeVisible)?;
            let top = top_layer(host.editor().doc());
            host.set_active_layer(top);
            Ok(())
        }
        "flatten" => exec_new_layer(host, &FlattenImage),
        "stamp-visible" => {
            let n = host.editor().doc().layer_count() + 1;
            exec_new_layer(
                host,
                &StampVisible {
                    name: format!("Stamp {n}"),
                },
            )
        }
        "rot-cw" => exec(host, &RotateImage { quarter_turns: 1 }),
        "rot-ccw" => exec(host, &RotateImage { quarter_turns: -1 }),
        "rot-180" => exec(host, &RotateImage { quarter_turns: 2 }),
        "img-flip-h" => exec(host, &FlipImage { horizontal: true }),
        "img-flip-v" => exec(host, &FlipImage { horizontal: false }),
        "crop" => {
            let doc = host.editor().doc();
            let rect = doc
                .selection
                .as_ref()
                .map(|s| s.tight_bounds(doc.canvas()))
                .ok_or("Crop to selection needs a selection")?;
            exec(host, &CropDocument { rect })
        }
        "reveal-all" => exec(host, &RevealAll),
        "adj-desaturate" => {
            let layer = host.active_layer().ok_or_else(need_layer)?;
            exec(host, &crate::adjust_cmds::Desaturate { layer })
        }
        "adjd-invert" => {
            let layer = host.active_layer().ok_or_else(need_layer)?;
            exec(
                host,
                &crate::adjust_cmds::ApplyAdjustment {
                    layer,
                    adjustment: Adjustment::Invert,
                },
            )
        }
        other if menu_recordable(other) => Err(format!("\"{}\" needs the app", menu_label(other))),
        other => Err(format!("\"{other}\" is not an action step")),
    }
}

/// Run one step.
pub fn play_step<H: ActionHost + ?Sized>(host: &mut H, step: &Step) -> Result<(), String> {
    match step {
        Step::Skipped { .. } => Ok(()),
        Step::Menu { id } => {
            if !menu_recordable(id) {
                return Err(format!("\"{id}\" is not an action step"));
            }
            host.run_menu(id)
        }
        Step::Adjust { adjustment } => {
            let mut cmd = AddAdjustmentLayer::new(adjustment.clone());
            cmd.above = host.active_layer();
            exec_new_layer(host, &cmd)
        }
        Step::Apply { adjustment } => {
            let layer = host
                .active_layer()
                .ok_or_else(|| format!("{} needs an active layer", adjustment.name()))?;
            exec(
                host,
                &crate::adjust_cmds::ApplyAdjustment {
                    layer,
                    adjustment: adjustment.clone(),
                },
            )
        }
        Step::Filter { filter } => {
            let layer = host.active_layer().ok_or("the filter needs an active layer")?;
            let l = host
                .editor()
                .doc()
                .layer(layer)
                .ok_or("the active layer is gone")?;
            let (smart, pixel) = (l.smart_layer().is_some(), l.pixels().is_some());
            if smart {
                exec(host, &AddSmartFilter::new(layer, filter.clone()))
            } else if pixel {
                exec(
                    host,
                    &ApplyFilter {
                        layer,
                        filter: filter.clone(),
                    },
                )
            } else {
                Err(format!("{} needs a pixel layer or smart object", filter.name()))
            }
        }
        Step::ImageSize {
            width,
            height,
            resolution,
        } => {
            if *width == 0 || *height == 0 {
                return Err("image size must be above zero".into());
            }
            let d = host.editor().doc();
            let same_px = (d.width, d.height) == (*width, *height);
            if same_px && resolution.is_none_or(|p| p == d.resolution) {
                return Ok(()); // already that size
            }
            exec(
                host,
                &ResizeImage {
                    width: *width,
                    height: *height,
                    resolution: *resolution,
                },
            )
        }
        Step::FitImage { long_edge } => {
            let (w, h) = {
                let d = host.editor().doc();
                (d.width, d.height)
            };
            let (nw, nh) = fit_long_edge(w, h, *long_edge);
            if (nw, nh) == (w, h) {
                return Ok(()); // already fits: never enlarge
            }
            exec(
                host,
                &ResizeImage {
                    width: nw,
                    height: nh,
                    resolution: None,
                },
            )
        }
        Step::CanvasSize {
            width,
            height,
            anchor,
        } => {
            if *width == 0 || *height == 0 {
                return Err("canvas size must be above zero".into());
            }
            exec(
                host,
                &ResizeCanvas {
                    width: *width,
                    height: *height,
                    anchor: (anchor[0].clamp(0.0, 1.0), anchor[1].clamp(0.0, 1.0)),
                },
            )
        }
        Step::RotateCanvas { degrees } => exec(host, &RotateCanvas { degrees: *degrees }),
        Step::Trim { transparent } => {
            let basis = if *transparent {
                TrimBasis::Transparent
            } else {
                TrimBasis::TopLeftColor
            };
            exec(host, &Trim { basis })
        }
    }
}

/// The size a `w × h` image gets from [`Step::FitImage`].
pub fn fit_long_edge(w: u32, h: u32, long_edge: u32) -> (u32, u32) {
    let long = w.max(h);
    if long_edge == 0 || long <= long_edge {
        return (w, h);
    }
    let s = long_edge as f64 / long as f64;
    (
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    )
}

/// Why playback stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayError {
    /// 1-based step number, as the panel numbers them.
    pub step: usize,
    pub what: String,
    pub message: String,
}

impl std::fmt::Display for PlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step {} ({}) failed: {}", self.step, self.what, self.message)
    }
}

/// Play `action` as ONE undo step labelled "Action: name". On an error the
/// steps already run are rolled back (no undo or redo entry is left) and
/// the error names the step that failed. Returns how many steps ran.
pub fn play<H: ActionHost + ?Sized>(host: &mut H, action: &Action) -> Result<usize, PlayError> {
    let active_before = host.active_layer();
    let (limits, before) = {
        let ed = host.editor();
        // Count the steps this playback pushes exactly: no trimming meanwhile.
        let limits = (ed.history_limit, ed.history_memory_limit);
        ed.history_limit = usize::MAX;
        ed.history_memory_limit = usize::MAX;
        (limits, ed.history().len())
    };
    let mut ran = 0;
    let mut failed = None;
    for (i, step) in action.steps.iter().enumerate() {
        if let Err(message) = play_step(host, step) {
            failed = Some(PlayError {
                step: i + 1,
                what: step.describe(),
                message,
            });
            break;
        }
        if !matches!(step, Step::Skipped { .. }) {
            ran += 1;
        }
    }
    let ed = host.editor();
    let pushed = ed.history().len().saturating_sub(before);
    match &failed {
        None => ed.squash_newest(pushed, &format!("Action: {}", action.name)),
        Some(_) => ed.rollback_newest(pushed),
    }
    (ed.history_limit, ed.history_memory_limit) = limits;
    match failed {
        None => Ok(ran),
        Some(e) => {
            host.set_active_layer(active_before);
            Err(e)
        }
    }
}

/// Actions shipped with Lumenply, marked [`Action::builtin`].
pub fn builtin_actions() -> Vec<Action> {
    let curve = |points: &[[f32; 2]]| Adjustment::Curves {
        points: points.to_vec(),
        channels: Default::default(),
    };
    vec![
        Action {
            name: "Web export prep".into(),
            steps: vec![
                Step::FitImage { long_edge: 2048 },
                Step::Menu { id: "flatten".into() },
                Step::Filter {
                    filter: Filter::Sharpen {
                        amount: 0.4,
                        radius: 1.0,
                    },
                },
            ],
            builtin: true,
        },
        Action {
            name: "Black & white contrast".into(),
            steps: vec![
                Step::Adjust {
                    adjustment: Adjustment::black_white_default(),
                },
                Step::Adjust {
                    // A gentle S curve.
                    adjustment: curve(&[[0.0, 0.0], [0.25, 0.18], [0.75, 0.84], [1.0, 1.0]]),
                },
            ],
            builtin: true,
        },
        Action {
            name: "Vintage fade".into(),
            steps: vec![
                Step::Adjust {
                    adjustment: Adjustment::photo_filter_default(),
                },
                Step::Adjust {
                    adjustment: Adjustment::HueSaturation {
                        hue: 0.0,
                        saturation: -0.3,
                        lightness: 0.0,
                        colorize: false,
                    },
                },
                Step::Adjust {
                    // Lifted blacks, softened whites.
                    adjustment: curve(&[[0.0, 0.08], [0.5, 0.52], [1.0, 0.94]]),
                },
            ],
            builtin: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::AddPixelLayer;
    use lumenply_tiles::{Raster, Rgba};

    /// A `w × h` document with one opaque pixel layer of `px`.
    fn solid(w: u32, h: u32, px: Rgba) -> Editor {
        let mut r = Raster::new(w, h);
        r.pixels.fill(px);
        let mut ed = Editor::new(Document::new(w, h));
        ed.execute(&AddPixelLayer::from_raster("Background", r, 0, 0))
            .unwrap();
        Editor::new(ed.doc().clone())
    }

    fn pixel(ed: &Editor, x: u32, y: u32) -> Rgba {
        let r = lumenply_render::composite_raster(ed.doc());
        r.pixels[(y * r.width + x) as usize]
    }

    fn approx(a: Rgba, b: [f32; 4]) {
        let got = [a.r, a.g, a.b, a.a];
        for (g, e) in got.iter().zip(b) {
            assert!((g - e).abs() < 1e-3, "got {got:?}, expected {b:?}");
        }
    }

    #[test]
    fn an_adjust_step_adds_a_layer_above_the_active_one_and_activates_it() {
        let mut ed = solid(4, 4, Rgba::new(0.0, 0.0, 0.0, 1.0));
        let mut host = CoreHost::new(&mut ed);
        let action = Action {
            name: "Invert".into(),
            steps: vec![Step::Adjust {
                adjustment: Adjustment::Invert,
            }],
            builtin: false,
        };
        assert_eq!(play(&mut host, &action), Ok(1));
        let new_id = host.active.unwrap();
        let doc = ed.doc();
        assert_eq!(doc.layers().len(), 2);
        assert_eq!(doc.layers()[1].id, new_id, "the new layer is active and on top");
        approx(pixel(&ed, 1, 1), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(ed.history(), vec!["Action: Invert"]);
    }

    #[test]
    fn an_apply_step_changes_the_active_layers_pixels() {
        let mut ed = solid(4, 4, Rgba::new(0.0, 0.0, 0.0, 1.0));
        let mut host = CoreHost::new(&mut ed);
        let action = Action {
            name: "Negative".into(),
            steps: vec![Step::Apply {
                adjustment: Adjustment::Invert,
            }],
            builtin: false,
        };
        assert_eq!(play(&mut host, &action), Ok(1));
        assert_eq!(ed.doc().layers().len(), 1, "no adjustment layer");
        approx(pixel(&ed, 0, 3), [1.0, 1.0, 1.0, 1.0]);
        // It survives a save: the JSON names the step "apply".
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains(r#""step":"apply""#), "{json}");
        let back: Action = serde_json::from_str(&json).unwrap();
        assert_eq!(back, action);
    }

    #[test]
    fn desaturate_and_invert_steps_play_without_the_app() {
        // Pink (1.0, 0.6, 0.6 in gamma): lightness (1.0 + 0.6) / 2 = 0.8,
        // inverted to 0.2 in gamma = 0.0331 linear.
        let g = |v: f32| ((v + 0.055) / 1.055).powf(2.4);
        let mut ed = solid(4, 4, Rgba::new(1.0, g(0.6), g(0.6), 1.0));
        let mut host = CoreHost::new(&mut ed);
        let action = Action {
            name: "Grey negative".into(),
            steps: vec![
                Step::Menu {
                    id: "adj-desaturate".into(),
                },
                Step::Menu {
                    id: "adjd-invert".into(),
                },
            ],
            builtin: false,
        };
        assert_eq!(play(&mut host, &action), Ok(2));
        let v = g(0.2);
        approx(pixel(&ed, 2, 2), [v, v, v, 1.0]);
        assert_eq!(ed.doc().layers().len(), 1, "applied to the pixels, no new layer");
        assert_eq!(ed.history(), vec!["Action: Grey negative"]);
    }

    #[test]
    fn a_filter_step_bakes_into_the_active_pixel_layer() {
        // Columns alternate black/white; a 2 px mosaic averages each pair.
        let mut r = Raster::new(4, 2);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let v = (i % 2) as f32;
            *p = Rgba::new(v, v, v, 1.0);
        }
        let mut ed = Editor::new(Document::new(4, 2));
        ed.execute(&AddPixelLayer::from_raster("Background", r, 0, 0))
            .unwrap();
        let mut host = CoreHost::new(&mut ed);
        let action = Action {
            name: "Mosaic".into(),
            steps: vec![Step::Filter {
                filter: Filter::Mosaic { size: 2.0 },
            }],
            builtin: false,
        };
        play(&mut host, &action).unwrap();
        assert_eq!(ed.doc().layers().len(), 1, "baked, no new layer");
        for x in 0..4 {
            approx(pixel(&ed, x, 0), [0.5, 0.5, 0.5, 1.0]);
        }
    }

    #[test]
    fn size_steps_give_exact_sizes_and_fit_never_enlarges() {
        let mut ed = solid(64, 32, Rgba::new(0.5, 0.5, 0.5, 1.0));
        let mut host = CoreHost::new(&mut ed);
        let steps = vec![
            Step::FitImage { long_edge: 16 },
            Step::FitImage { long_edge: 100 },
            Step::CanvasSize {
                width: 20,
                height: 10,
                anchor: [0.0, 0.0],
            },
            Step::ImageSize {
                width: 10,
                height: 5,
                resolution: None,
            },
            Step::ImageSize {
                width: 10,
                height: 5,
                resolution: Some(300.0),
            },
        ];
        let action = Action {
            name: "Sizes".into(),
            steps,
            builtin: false,
        };
        assert_eq!(play(&mut host, &action), Ok(5));
        assert_eq!((ed.doc().width, ed.doc().height), (10, 5));
        // The repeated size only re-tags: 300 ppi, no resample.
        assert_eq!(ed.doc().resolution, 300.0);
        assert_eq!(fit_long_edge(6000, 4000, 2048), (2048, 1365));
        assert_eq!(fit_long_edge(1000, 800, 2048), (1000, 800));
        // One undo step takes the whole action back.
        assert_eq!(ed.history().len(), 1);
        ed.undo();
        assert_eq!((ed.doc().width, ed.doc().height), (64, 32));
    }

    #[test]
    fn a_failing_step_rolls_everything_back_and_names_the_step() {
        let mut ed = solid(8, 8, Rgba::new(0.5, 0.5, 0.5, 1.0));
        ed.execute(&SetSelection { selection: None }).unwrap();
        let history = ed.history().len();
        let mut host = CoreHost::new(&mut ed);
        let action = Action {
            name: "Breaks".into(),
            steps: vec![
                Step::Menu { id: "rot-cw".into() },
                Step::Skipped {
                    what: "Paint stroke".into(),
                },
                Step::Menu { id: "crop".into() },
            ],
            builtin: false,
        };
        let err = play(&mut host, &action).unwrap_err();
        assert_eq!(err.step, 3);
        assert_eq!(err.what, "Crop to selection");
        assert!(err.to_string().contains("step 3 (Crop to selection) failed"));
        assert_eq!(ed.history().len(), history, "rolled back");
        assert!(!ed.can_redo(), "and nothing left to redo");
        // An unknown id is refused, not run.
        let mut host = CoreHost::new(&mut ed);
        let bad = Action {
            name: "Bad".into(),
            steps: vec![Step::Menu { id: "open".into() }],
            builtin: false,
        };
        assert_eq!(play(&mut host, &bad).unwrap_err().step, 1);
    }

    #[test]
    fn menu_steps_flatten_rotate_and_duplicate() {
        let mut ed = solid(6, 4, Rgba::new(1.0, 0.0, 0.0, 1.0));
        let mut host = CoreHost::new(&mut ed);
        let action = Action {
            name: "Menu".into(),
            steps: ["duplicate-layer", "rot-cw", "flatten"]
                .iter()
                .map(|id| Step::Menu { id: id.to_string() })
                .collect(),
            builtin: false,
        };
        assert_eq!(play(&mut host, &action), Ok(3));
        assert_eq!((ed.doc().width, ed.doc().height), (4, 6));
        assert_eq!(ed.doc().layers().len(), 1);
        approx(pixel(&ed, 2, 3), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn actions_round_trip_through_json() {
        let mut all = builtin_actions();
        for a in &mut all {
            a.builtin = false;
        }
        all[0].steps.push(Step::Skipped {
            what: "Paint stroke".into(),
        });
        all[0].steps.push(Step::RotateCanvas { degrees: 12.5 });
        all[0].steps.push(Step::Trim { transparent: true });
        let json = to_json(&all);
        assert!(json.contains("\"step\": \"fit-image\""), "{json}");
        let back = from_json(&json).unwrap();
        assert_eq!(back, all);
        // Built-ins are never written to the user's file.
        assert_eq!(
            from_json(&to_json(&builtin_actions())).unwrap(),
            Vec::<Action>::new()
        );
        // A single action and a bare list load too.
        let one = serde_json::to_string(&all[1]).unwrap();
        assert_eq!(from_json(&one).unwrap(), vec![all[1].clone()]);
        assert_eq!(from_json(&format!("[{one}]")).unwrap().len(), 1);
    }

    #[test]
    fn unknown_steps_and_bad_files_are_clear_errors() {
        let e = from_json(r#"{"actions":[{"name":"Mine","steps":[{"step":"teleport"}]}]}"#).unwrap_err();
        assert!(e.contains("\"Mine\"") && e.contains("teleport"), "{e}");
        let e =
            from_json(r#"{"name":"X","steps":[{"step":"filter","filter":{"type":"nope"}}]}"#).unwrap_err();
        assert!(e.contains("nope"), "{e}");
        assert!(from_json("not json").unwrap_err().contains("not valid JSON"));
        assert!(from_json("{}").is_err());
        assert!(from_json("42").is_err());
    }

    #[test]
    fn the_builtins_play_on_a_small_image() {
        for a in builtin_actions() {
            let mut ed = solid(32, 16, Rgba::new(0.4, 0.3, 0.2, 1.0));
            let mut host = CoreHost::new(&mut ed);
            assert!(play(&mut host, &a).is_ok(), "{} failed", a.name);
            assert_eq!(ed.history(), vec![format!("Action: {}", a.name)]);
        }
        // B&W: equal channels afterwards.
        let mut ed = solid(4, 4, Rgba::new(0.6, 0.2, 0.1, 1.0));
        play(&mut CoreHost::new(&mut ed), &builtin_actions()[1]).unwrap();
        let p = pixel(&ed, 0, 0);
        assert!((p.r - p.g).abs() < 1e-4 && (p.g - p.b).abs() < 1e-4, "{p:?}");
    }
}
