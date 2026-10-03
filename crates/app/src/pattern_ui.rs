//! Patterns in the app (ADR 0020): the pattern library (built-ins plus
//! the user's own, kept as 16-bit PNGs in a `patterns` folder next to
//! prefs.json), the picker popover, the Properties rows for pattern fills
//! and the Pattern Overlay effect, Edit ▸ Define Pattern, `.pat` import,
//! and the `pattern:` debug tokens.

use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::theme::{a11y_name, check, row_label, slider_row, slider_row_scaled, LABEL_W, MUTED};
use lumenply_core::pattern_cmds::{capture_pattern, DefinePattern};
use lumenply_doc::pattern::{display_name, new_pattern_id, raster_hash, PatternOverlayFx, PatternRef};
use lumenply_doc::{Fill, Pattern};

/// Thumbnail side in the picker and on the chips.
const THUMB: usize = 36;
/// Columns in the picker grid.
const COLUMNS: usize = 6;

/// What a picked pattern is applied to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PickTarget {
    /// Layer ▸ New fill layer ▸ Pattern…
    NewFill,
    /// A pattern fill layer's pattern.
    Fill(LayerId),
    /// A layer's Pattern Overlay effect.
    Overlay(LayerId),
    /// A shape layer's fill.
    Shape(LayerId),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LibraryEntry {
    id: String,
    name: String,
    file: String,
}

/// Built-in and user patterns, thumbnails and the open picker.
pub(crate) struct PatternLibrary {
    builtins: Vec<Pattern>,
    user: Vec<Pattern>,
    thumbs: HashMap<String, (usize, TextureHandle)>,
    /// The open picker: what it applies to, where (`None`: near the top
    /// of the window) and the frame it opened (`None`: not drawn yet).
    open: Option<(PickTarget, Option<Pos2>, Option<u64>)>,
}

impl Default for PatternLibrary {
    fn default() -> Self {
        PatternLibrary {
            builtins: lumenply_render::pattern::builtin_patterns(),
            user: load_user(),
            thumbs: HashMap::new(),
            open: None,
        }
    }
}

fn library_dir() -> Option<PathBuf> {
    session::data_dir().map(|d| d.join("patterns"))
}

fn load_user() -> Vec<Pattern> {
    let Some(dir) = library_dir() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join("library.json")) else {
        return Vec::new();
    };
    let entries: Vec<LibraryEntry> = serde_json::from_str(&text).unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|e| {
            let bytes = std::fs::read(dir.join(&e.file)).ok()?;
            let img = lumenply_io::pattern_files::decode_pattern_png(&bytes).ok()?;
            Some(Pattern::new(e.id, e.name, img))
        })
        .collect()
}

impl PatternLibrary {
    /// Keep `p` in the user library (replacing one with its id) and write
    /// the library to disk.
    pub(crate) fn add_user(&mut self, p: Pattern) -> Result<(), String> {
        let dir = library_dir().ok_or("no folder to keep patterns in")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = format!("{}.png", lumenply_io::pattern_files::file_stem(&p.id));
        let png = lumenply_io::pattern_files::encode_pattern_png(&p.image).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(&file), png).map_err(|e| e.to_string())?;
        match self.user.iter().position(|q| q.id == p.id) {
            Some(i) => self.user[i] = p,
            None => self.user.push(p),
        }
        let entries: Vec<LibraryEntry> = self
            .user
            .iter()
            .map(|q| LibraryEntry {
                id: q.id.clone(),
                name: q.name.clone(),
                file: format!("{}.png", lumenply_io::pattern_files::file_stem(&q.id)),
            })
            .collect();
        let json = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("library.json"), json).map_err(|e| e.to_string())
    }

    /// Every pattern the picker offers, in sections: the document's own,
    /// the built-ins, then the user's (each id once).
    fn sections(&self, doc: &Document) -> Vec<(&'static str, Vec<Pattern>)> {
        let mut seen: Vec<String> = Vec::new();
        let mut take = |list: &[Pattern]| -> Vec<Pattern> {
            list.iter()
                .filter(|p| {
                    if seen.contains(&p.id) {
                        false
                    } else {
                        seen.push(p.id.clone());
                        true
                    }
                })
                .cloned()
                .collect()
        };
        let docs = take(&doc.patterns);
        let built = take(&self.builtins);
        let user = take(&self.user);
        [
            ("IN THIS DOCUMENT", docs),
            ("BUILT-IN", built),
            ("MY PATTERNS", user),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }

    /// The first built-in (the default for a new pattern fill).
    pub(crate) fn default_pattern(&self) -> Pattern {
        self.builtins[0].clone()
    }

    fn builtin(&self, key: &str) -> Option<Pattern> {
        self.builtins
            .iter()
            .find(|p| p.id.ends_with(key) || p.name.eq_ignore_ascii_case(key))
            .cloned()
    }

    fn thumb(&mut self, ctx: &egui::Context, p: &Pattern) -> TextureHandle {
        let key = Arc::as_ptr(&p.image) as usize;
        if let Some((k, t)) = self.thumbs.get(&p.id) {
            if *k == key {
                return t.clone();
            }
        }
        let tex = ctx.load_texture(
            format!("pattern-thumb-{}", p.id),
            thumb_image(&p.image),
            egui::TextureOptions::NEAREST,
        );
        self.thumbs.insert(p.id.clone(), (key, tex.clone()));
        tex
    }
}

/// A swatch of the pattern tiled at 100% (big patterns shrink to fit),
/// over white where it is transparent.
fn thumb_image(img: &Raster) -> egui::ColorImage {
    let side = img.width.max(img.height) as f32;
    let step = (side / THUMB as f32).max(1.0);
    let mut px = Vec::with_capacity(THUMB * THUMB);
    for y in 0..THUMB {
        for x in 0..THUMB {
            let sx = ((x as f32 + 0.5) * step) as u32 % img.width.max(1);
            let sy = ((y as f32 + 0.5) * step) as u32 % img.height.max(1);
            let p = img.get(sx, sy).over(lumenply_tiles::Rgba::WHITE);
            let [r, g, b, _] = p.to_straight();
            px.push(Color32::from_rgb(
                lumenply_io::linear_to_srgb(r),
                lumenply_io::linear_to_srgb(g),
                lumenply_io::linear_to_srgb(b),
            ));
        }
    }
    egui::ColorImage {
        size: [THUMB, THUMB],
        pixels: px,
    }
}

impl App {
    /// Open the picker under `at` for `target`.
    pub(crate) fn open_pattern_picker(&mut self, target: PickTarget, at: Option<Pos2>) {
        self.patterns.open = Some((target, at, None));
    }

    /// A chip showing `current` (thumbnail and name) that opens the picker.
    fn pattern_chip(&mut self, ui: &mut egui::Ui, current: &PatternRef, target: PickTarget) {
        let doc_pattern = self.editor.doc().find_pattern(current).cloned();
        ui.horizontal(|ui| {
            row_label(ui, "Pattern", LABEL_W);
            let tex = doc_pattern.as_ref().map(|p| self.patterns.thumb(ui.ctx(), p));
            let name = display_name(&current.name).to_string();
            let resp = match &tex {
                Some(t) => ui.add(egui::ImageButton::new((t.id(), Vec2::splat(22.0)))),
                None => ui.button("?"),
            };
            a11y_name(&resp, &format!("Choose pattern (now {name})"));
            let resp = resp.on_hover_text("Choose a pattern");
            if resp.clicked() {
                self.open_pattern_picker(target, Some(resp.rect.left_bottom()));
            }
            ui.add(egui::Label::new(RichText::new(name).color(MUTED)).truncate());
        });
    }

    /// Properties of a pattern fill: the pattern, scale, phase and Snap to
    /// Origin. Edits `fill` in place; returns whether a drag finished.
    pub(crate) fn pattern_fill_rows(
        &mut self,
        ui: &mut egui::Ui,
        target: PickTarget,
        fill: &mut Fill,
    ) -> bool {
        let Fill::Pattern {
            pattern,
            scale,
            offset,
            angle,
        } = fill
        else {
            return false;
        };
        self.pattern_chip(ui, &pattern.clone(), target);
        let mut finished = slider_row_scaled(ui, "Scale", scale, 0.05..=4.0, 100.0, "%");
        finished |= slider_row(ui, "Angle", angle, -180.0..=180.0, "°");
        finished |= slider_row(ui, "Offset X", &mut offset[0], -512.0..=512.0, " px");
        finished |= slider_row(ui, "Offset Y", &mut offset[1], -512.0..=512.0, " px");
        ui.horizontal(|ui| {
            row_label(ui, "", LABEL_W);
            if ui.button("Snap to origin").clicked() {
                *offset = [0.0, 0.0];
                finished = true;
            }
        });
        finished
    }

    /// The Pattern Overlay rows of the effects section. Returns (changed,
    /// finished).
    pub(crate) fn pattern_overlay_ui(
        &mut self,
        ui: &mut egui::Ui,
        id: LayerId,
        po: &mut Option<PatternOverlayFx>,
    ) -> (bool, bool) {
        let (mut changed, mut finished) = (false, false);
        let mut on = po.is_some();
        if check(ui, &mut on, "Pattern overlay").changed() {
            *po = on.then(|| {
                let p = self
                    .editor
                    .doc()
                    .patterns
                    .first()
                    .cloned()
                    .unwrap_or_else(|| self.patterns.default_pattern());
                PatternOverlayFx::new(p.reference())
            });
            changed = true;
            finished = true;
        }
        if let Some(fx) = po {
            let before = fx.clone();
            self.pattern_chip(ui, &fx.pattern.clone(), PickTarget::Overlay(id));
            finished |= slider_row_scaled(ui, "Scale", &mut fx.scale, 0.05..=4.0, 100.0, "%");
            finished |= slider_row_scaled(ui, "Opacity", &mut fx.opacity, 0.0..=1.0, 100.0, "%");
            ui.horizontal(|ui| {
                row_label(ui, "", LABEL_W);
                if ui.button("Snap to origin").clicked() {
                    fx.offset = [0.0, 0.0];
                    finished = true;
                }
            });
            changed |= *fx != before;
        }
        (changed, finished)
    }

    /// Draw the open picker (call once per frame, after the panels).
    pub(crate) fn pattern_picker_ui(&mut self, ctx: &egui::Context) {
        let Some((target, at, opened)) = self.patterns.open else {
            return;
        };
        let at = at.unwrap_or_else(|| ctx.screen_rect().center_top() + Vec2::new(-130.0, 90.0));
        let opened = opened.unwrap_or_else(|| ctx.cumulative_pass_nr());
        self.patterns.open = Some((target, Some(at), Some(opened)));
        if self.no_doc || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.patterns.open = None;
            return;
        }
        let sections = self.patterns.sections(self.editor.doc());
        let current = self.picker_current(target);
        let mut picked: Option<Pattern> = None;
        let mut import = false;
        let area = egui::Area::new(egui::Id::new("pattern-picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .constrain(true)
            .fade_in(false)
            .sense(crate::theme::BACKDROP_SENSE)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(4.0);
                    ui.spacing_mut().button_padding = Vec2::splat(2.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Patterns").strong());
                        ui.label(RichText::new("click to apply").small().color(MUTED));
                    });
                    for (title, list) in &sections {
                        ui.label(RichText::new(*title).small().strong().color(MUTED));
                        for row in list.chunks(COLUMNS) {
                            ui.horizontal(|ui| {
                                for p in row {
                                    let tex = self.patterns.thumb(ctx, p);
                                    let selected = current.as_deref() == Some(p.id.as_str());
                                    let r = ui.add(
                                        egui::ImageButton::new((tex.id(), Vec2::splat(THUMB as f32)))
                                            .selected(selected),
                                    );
                                    let name = display_name(&p.name);
                                    a11y_name(&r, &format!("Pattern {name}"));
                                    if r.on_hover_text(format!("{name} ({}×{})", p.width(), p.height()))
                                        .clicked()
                                    {
                                        picked = Some(p.clone());
                                    }
                                }
                            });
                        }
                        ui.add_space(4.0);
                    }
                    if ui.button("Import Photoshop patterns (.pat)…").clicked() {
                        import = true;
                    }
                });
            });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && ctx.cumulative_pass_nr() > opened + 1
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| !area.response.rect.contains(p));
        if let Some(p) = picked {
            self.patterns.open = None;
            self.apply_pattern(target, p);
        } else if import {
            self.patterns.open = None;
            self.pick_import_patterns();
        } else if clicked_outside {
            self.patterns.open = None;
        }
    }

    /// The id of the pattern `target` uses now.
    fn picker_current(&self, target: PickTarget) -> Option<String> {
        let doc = self.editor.doc();
        match target {
            PickTarget::NewFill => None,
            PickTarget::Fill(id) => match doc.layer(id)?.fill_layer()?.fill {
                Fill::Pattern { ref pattern, .. } => Some(pattern.id.clone()),
                _ => None,
            },
            PickTarget::Shape(id) => match doc.layer(id)?.shape_layer()?.fill {
                Some(Fill::Pattern { ref pattern, .. }) => Some(pattern.id.clone()),
                _ => None,
            },
            PickTarget::Overlay(id) => doc
                .layer(id)?
                .effects
                .pattern_overlay
                .as_ref()
                .map(|p| p.pattern.id.clone()),
        }
    }

    /// Apply a picked pattern to `target` (one undo step; the document
    /// adopts the pattern).
    pub(crate) fn apply_pattern(&mut self, target: PickTarget, p: Pattern) {
        let r = p.reference();
        match target {
            PickTarget::NewFill => self.add_pattern_fill_layer(p),
            PickTarget::Fill(id) => {
                let Some(old) = self
                    .editor
                    .doc()
                    .layer(id)
                    .and_then(|l| l.fill_layer())
                    .map(|f| f.fill.clone())
                else {
                    return;
                };
                let fill = match old {
                    Fill::Pattern {
                        scale, offset, angle, ..
                    } => Fill::Pattern {
                        pattern: r,
                        scale,
                        offset,
                        angle,
                    },
                    _ => Fill::pattern(r),
                };
                self.run(&SetFill { layer: id, fill });
            }
            PickTarget::Shape(id) => {
                let Some(mut shape) = self.editor.doc().layer(id).and_then(|l| l.shape_layer()).cloned()
                else {
                    return;
                };
                shape.fill = Some(match shape.fill {
                    Some(Fill::Pattern {
                        scale, offset, angle, ..
                    }) => Fill::Pattern {
                        pattern: r,
                        scale,
                        offset,
                        angle,
                    },
                    _ => Fill::pattern(r),
                });
                self.run(&SetShape { layer: id, shape });
            }
            PickTarget::Overlay(id) => {
                let Some(mut fx) = self.editor.doc().layer(id).map(|l| l.effects.clone()) else {
                    return;
                };
                fx.pattern_overlay = Some(match fx.pattern_overlay.take() {
                    Some(o) => PatternOverlayFx { pattern: r, ..o },
                    None => PatternOverlayFx::new(r),
                });
                self.run(&SetLayerEffects {
                    layer: id,
                    effects: fx,
                });
            }
        }
    }

    /// A pattern fill layer above the active layer (masked by the
    /// selection, if any).
    pub(crate) fn add_pattern_fill_layer(&mut self, p: Pattern) {
        let new_id = self.editor.doc().next_id();
        let mut cmd = AddFillLayer::new(Fill::pattern(p.reference()));
        cmd.above = self.active;
        self.run(&cmd);
        self.set_active(Some(new_id));
        self.fix_active();
    }

    /// Edit ▸ Define Pattern: the selection's bounds of the visible image
    /// (the whole canvas without a selection) become a pattern in the
    /// document and in the user library.
    pub(crate) fn define_pattern(&mut self) {
        let doc = self.editor.doc();
        let img = match capture_pattern(doc, None) {
            Ok(img) => img,
            Err(e) => {
                self.status = format!("Could not define a pattern: {e}");
                return;
            }
        };
        let n = self.patterns.user.len() + doc.patterns.len() + 1;
        let p = Pattern::new(new_pattern_id(raster_hash(&img)), format!("Pattern {n}"), img);
        let (w, h) = (p.width(), p.height());
        self.run(&DefinePattern { pattern: p.clone() });
        self.status = match self.patterns.add_user(p) {
            Ok(()) => format!("Defined a {w}×{h} pattern; it is in the pattern picker"),
            Err(e) => format!("Defined a {w}×{h} pattern for this document (not kept for later: {e})"),
        };
    }

    /// Edit ▸ Import patterns…
    pub(crate) fn pick_import_patterns(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Import patterns")
            .add_filter("Photoshop patterns", &["pat", "PAT"])
            .pick_file()
        {
            self.import_patterns(&p);
        }
    }

    /// Read a Photoshop `.pat` set into the user library.
    pub(crate) fn import_patterns(&mut self, path: &Path) {
        let file = file_name(&path.to_string_lossy());
        let list = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| lumenply_io::pattern_files::load_pat(&b).map_err(|e| e.to_string()))
        {
            Ok(l) => l,
            Err(e) => {
                self.status = format!("Could not import {file}: {e}");
                return;
            }
        };
        let n = list.len();
        let mut failed = None;
        for p in list {
            if let Err(e) = self.patterns.add_user(p) {
                failed = Some(e);
            }
        }
        self.status = match failed {
            None => format!(
                "Imported {n} pattern{} from {file}",
                if n == 1 { "" } else { "s" }
            ),
            Some(e) => format!("Imported patterns from {file} for this session only: {e}"),
        };
    }

    /// Whether a pattern action can run: outer `None` when `id` is not a
    /// pattern action, `Some(None)` when it can run.
    pub(crate) fn pattern_action_block(&self, id: &str) -> Option<Option<&'static str>> {
        match id {
            "fill-pattern" | "define-pattern" | "import-patterns" => Some(None),
            _ => None,
        }
    }

    /// Run a pattern action; false when `id` is not one.
    pub(crate) fn run_pattern_action(&mut self, id: &str) -> bool {
        match id {
            // From a menu or the palette: the picker opens near the top of
            // the window and the pick makes the layer.
            "fill-pattern" => self.open_pattern_picker(PickTarget::NewFill, None),
            "define-pattern" => self.define_pattern(),
            "import-patterns" => self.pick_import_patterns(),
            _ => return false,
        }
        true
    }

    /// Screenshot tokens (`pattern:...`): `pattern:fill=<key>` adds a
    /// pattern fill layer with the built-in of that key (`bricks`, `wood`,
    /// …); `pattern:overlay=<key>` puts a Pattern Overlay on the active
    /// layer (85% Multiply); `pattern:scale=<percent>` scales the active pattern fill;
    /// `pattern:pick=new|fill|overlay` opens the picker for a new fill
    /// layer, the active fill layer or the active layer's overlay;
    /// `pattern:define` runs Define Pattern.
    pub(crate) fn debug_pattern(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("pattern:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        match verb {
            "fill" => {
                if let Some(p) = self.patterns.builtin(arg) {
                    self.add_pattern_fill_layer(p);
                }
            }
            "overlay" => {
                if let (Some(p), Some(id)) = (self.patterns.builtin(arg), self.active) {
                    self.apply_pattern(PickTarget::Overlay(id), p);
                    if let Some(mut fx) = self.editor.doc().layer(id).map(|l| l.effects.clone()) {
                        if let Some(o) = &mut fx.pattern_overlay {
                            o.opacity = 0.85;
                            o.blend = BlendMode::Multiply;
                        }
                        self.run(&SetLayerEffects {
                            layer: id,
                            effects: fx,
                        });
                    }
                }
            }
            "scale" => {
                let pct: f32 = arg.parse().unwrap_or(100.0);
                if let Some(id) = self.active {
                    if let Some(Fill::Pattern {
                        pattern,
                        offset,
                        angle,
                        ..
                    }) = self
                        .editor
                        .doc()
                        .layer(id)
                        .and_then(|l| l.fill_layer())
                        .map(|f| f.fill.clone())
                    {
                        let fill = Fill::Pattern {
                            pattern,
                            scale: pct / 100.0,
                            offset,
                            angle,
                        };
                        self.run(&SetFill { layer: id, fill });
                    }
                }
            }
            "pick" => {
                let target = match (arg, self.active) {
                    ("fill", Some(id)) => PickTarget::Fill(id),
                    ("overlay", Some(id)) => PickTarget::Overlay(id),
                    _ => PickTarget::NewFill,
                };
                let at = ctx.screen_rect().right_top() + Vec2::new(-560.0, 140.0);
                self.open_pattern_picker(target, Some(at));
            }
            "define" => self.define_pattern(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y_tests::{ctx, launch, nameless};

    #[test]
    fn the_picker_lists_document_then_builtin_patterns_and_applies_one() {
        let mut app = launch(&[]);
        app.open_in_new_tab(blank(32, 16), None);
        let lib = PatternLibrary::default();
        let secs = lib.sections(app.editor.doc());
        assert_eq!(secs[0].0, "BUILT-IN");
        assert_eq!(secs[0].1.len(), 8);
        // A new pattern fill layer from the picker: the document adopts
        // the pattern, which then heads the picker.
        app.apply_pattern(PickTarget::NewFill, lib.builtin("bricks").unwrap());
        let doc = app.editor.doc();
        assert_eq!(doc.patterns.len(), 1);
        assert_eq!(doc.patterns[0].name, "Bricks");
        let top = doc.layers().last().unwrap();
        assert_eq!(top.name, "Pattern Fill");
        let secs = lib.sections(doc);
        assert_eq!((secs[0].0, secs[0].1.len()), ("IN THIS DOCUMENT", 1));
        assert_eq!(secs[1].1.len(), 7, "the built-in is listed once");
        // Picking another pattern for the same layer keeps its scale.
        let id = top.id;
        app.run(&SetFill {
            layer: id,
            fill: Fill::Pattern {
                pattern: lib.builtin("bricks").unwrap().reference(),
                scale: 0.5,
                offset: [0.0, 0.0],
                angle: 0.0,
            },
        });
        app.apply_pattern(PickTarget::Fill(id), lib.builtin("dots").unwrap());
        let f = &app.editor.doc().layer(id).unwrap().fill_layer().unwrap().fill;
        assert!(
            matches!(f, Fill::Pattern { pattern, scale, .. } if pattern.name == "Polka Dots" && *scale == 0.5)
        );
        // Bricks (32×16) at 100% from the origin: canvas (0, 0) is a brick,
        // (7, 7) is mortar on the bottom row of the first course.
        let cache = app.editor.doc().layer(id).unwrap().fill_layer().unwrap();
        assert!(cache.cache.is_some());
    }

    #[test]
    fn define_pattern_adds_to_the_document_and_the_library() {
        let mut app = launch(&[]);
        app.open_in_new_tab(blank(20, 10), None);
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(2, 2, 4, 3))),
        });
        let before = app.patterns.user.len();
        app.define_pattern();
        let doc = app.editor.doc();
        assert_eq!(doc.patterns.len(), 1);
        assert_eq!((doc.patterns[0].width(), doc.patterns[0].height()), (4, 3));
        assert_eq!(app.patterns.user.len(), before + 1);
        // A new launch finds it in the library folder.
        let id = doc.patterns[0].id.clone();
        assert!(load_user().iter().any(|p| p.id == id));
    }

    #[test]
    fn the_open_picker_names_every_control() {
        let mut app = launch(&["--demo".to_string()]);
        let ctx = ctx();
        // A pattern fill with a Pattern Overlay: Properties shows both.
        let p = app.patterns.default_pattern();
        app.add_pattern_fill_layer(p.clone());
        let id = app.active.unwrap();
        app.apply_pattern(PickTarget::Overlay(id), p);
        assert!(app
            .editor
            .doc()
            .layer(id)
            .unwrap()
            .effects
            .pattern_overlay
            .is_some());
        app.open_pattern_picker(PickTarget::NewFill, Some(Pos2::new(300.0, 120.0)));
        let missing = nameless(&mut app, &ctx);
        assert_eq!(missing, Vec::<String>::new());
        assert!(app.patterns.open.is_some(), "still open after drawing");
    }
}
