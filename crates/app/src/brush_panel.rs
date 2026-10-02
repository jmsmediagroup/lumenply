//! Brush settings: a floating panel from the brush options bar with the
//! tip picker (the round tip, the generated built-ins and imported tips),
//! tip shape, shape dynamics, scattering and transfer; Photoshop brush
//! import (File ▸ Import brushes…) into a `brushes` folder beside
//! prefs.json; and the `brush:` debug tokens that paint test strokes.

use std::path::Path;
use std::sync::Arc;

use lumenply_core::brush_tip::{builtin_tip, BUILTIN_TIPS, MAX_SCATTER};
use lumenply_io::abr::AbrShape;

use super::*;

/// Thumbnail texture side (drawn at `CELL` points: sharp on 2× screens).
const THUMB: u32 = 72;
/// Side of one cell in the tip grid.
const CELL: f32 = 40.0;
/// Cells per grid row.
const PER_ROW: usize = 6;
/// Label column of the panel's slider rows.
const LABEL: f32 = 116.0;
/// Width of each of the panel's two columns.
const COL_W: f32 = 268.0;

/// Spacing the built-in tips switch to when picked (fraction of the
/// radius): textured tips read best with their dabs apart.
fn builtin_spacing(name: &str) -> f32 {
    match name {
        "Chalk" => 0.3,
        "Spatter" => 0.8,
        "Grass" => 0.7,
        "Dry brush" => 0.08,
        _ => 0.9,
    }
}

/// One tip in the picker.
pub(crate) struct TipEntry {
    /// `builtin:<name>` or `abr-<hash>`.
    pub id: String,
    pub name: String,
    /// Spacing to switch to on picking (fraction of the radius).
    pub spacing: Option<f32>,
    /// An imported tip's own diameter in pixels: picking it sets the size.
    pub size: Option<f32>,
    file: Option<PathBuf>,
    /// Loaded while it is (or was) on the brush; the grid only keeps
    /// thumbnails, so a big imported library doesn't sit in memory.
    tip: Option<Arc<BrushTip>>,
    thumb_gray: Option<Vec<u8>>,
    thumb: Option<egui::TextureHandle>,
    /// Loading failed once: don't retry every frame.
    broken: bool,
}

impl TipEntry {
    fn new(id: String, name: String) -> Self {
        TipEntry {
            id,
            name,
            spacing: None,
            size: None,
            file: None,
            tip: None,
            thumb_gray: None,
            thumb: None,
            broken: false,
        }
    }
}

/// The tips the picker offers, and which one is on the brush.
pub(crate) struct BrushLibrary {
    pub tips: Vec<TipEntry>,
    /// Id of the tip on the brush ("" = the round tip).
    pub selected: String,
    /// Tips removed this session, so saving the index drops them even
    /// when another window has rewritten it meanwhile.
    removed: Vec<String>,
}

/// One imported tip in `brushes/index.json`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct IndexEntry {
    id: String,
    name: String,
    file: String,
    #[serde(default)]
    spacing: Option<f32>,
    #[serde(default)]
    size: Option<f32>,
}

fn brushes_dir() -> Option<PathBuf> {
    session::data_dir().map(|d| d.join("brushes"))
}

fn read_index() -> Vec<IndexEntry> {
    brushes_dir()
        .and_then(|d| std::fs::read_to_string(d.join("index.json")).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn load_png_tip(path: &Path, name: &str) -> Option<Arc<BrushTip>> {
    let img = image::open(path).ok()?.to_luma16();
    let (w, h) = img.dimensions();
    BrushTip::from_gray16(name, w, h, img.as_raw()).ok().map(Arc::new)
}

/// FNV-1a over a tip's size and pixels: re-importing a set finds the
/// tips it already has.
fn tip_hash(w: u32, h: u32, gray: &[u16]) -> u64 {
    let mut h64: u64 = 0xCBF2_9CE4_8422_2325;
    let mut eat = |b: u8| {
        h64 ^= b as u64;
        h64 = h64.wrapping_mul(0x0100_0000_01B3);
    };
    for b in w.to_le_bytes().into_iter().chain(h.to_le_bytes()) {
        eat(b);
    }
    for v in gray {
        for b in v.to_le_bytes() {
            eat(b);
        }
    }
    h64
}

impl BrushLibrary {
    /// The built-ins (generated on first use) and the imported tips listed
    /// in the data folder.
    pub(crate) fn load() -> Self {
        let mut tips: Vec<TipEntry> = BUILTIN_TIPS
            .iter()
            .map(|&n| {
                let mut e = TipEntry::new(format!("builtin:{n}"), n.into());
                e.spacing = Some(builtin_spacing(n));
                e
            })
            .collect();
        let dir = brushes_dir();
        for e in read_index() {
            let mut t = TipEntry::new(e.id, e.name);
            t.spacing = e.spacing;
            t.size = e.size;
            t.file = dir.as_ref().map(|d| d.join(&e.file));
            tips.push(t);
        }
        BrushLibrary {
            tips,
            selected: String::new(),
            removed: Vec::new(),
        }
    }

    pub(crate) fn index_of(&self, id: &str) -> Option<usize> {
        self.tips.iter().position(|t| t.id == id)
    }

    /// The tip at `i`, generated or loaded (and kept) on first use.
    pub(crate) fn tip(&mut self, i: usize) -> Option<Arc<BrushTip>> {
        let e = &mut self.tips[i];
        if e.tip.is_none() && !e.broken {
            e.tip = match e.id.strip_prefix("builtin:") {
                Some(name) => builtin_tip(name),
                None => e.file.as_deref().and_then(|p| load_png_tip(p, &e.name)),
            };
            e.broken = e.tip.is_none();
        }
        e.tip.clone()
    }

    /// Thumbnail texture of tip `i`; an imported tip is read for it and
    /// let go again unless it is on the brush.
    fn thumb(&mut self, ctx: &egui::Context, i: usize) -> Option<egui::TextureHandle> {
        if let Some(t) = &self.tips[i].thumb {
            return Some(t.clone());
        }
        if self.tips[i].broken {
            return None;
        }
        let gray = match self.tips[i].thumb_gray.take() {
            Some(g) => g,
            None => {
                let e = &self.tips[i];
                let tip = match (&e.tip, e.id.strip_prefix("builtin:")) {
                    (Some(t), _) => Some(t.clone()),
                    (None, Some(_)) => self.tip(i),
                    (None, None) => e.file.as_deref().and_then(|p| load_png_tip(p, &e.name)),
                };
                let Some(tip) = tip else {
                    self.tips[i].broken = true;
                    return None;
                };
                tip.thumbnail(THUMB)
            }
        };
        let tex = ctx.load_texture(
            format!("brush-tip-{}", self.tips[i].id),
            thumb_image(&gray),
            egui::TextureOptions::LINEAR,
        );
        self.tips[i].thumb = Some(tex.clone());
        Some(tex)
    }

    /// Write `brushes/index.json`: what is on disk (another window may
    /// have added tips since this one loaded it), minus the tips removed
    /// here, plus the ones added here.
    fn save_index(&self) {
        let Some(dir) = brushes_dir() else {
            return;
        };
        let mut list = read_index();
        list.retain(|e| !self.removed.contains(&e.id));
        for t in self.tips.iter().filter(|t| !t.id.starts_with("builtin:")) {
            if !list.iter().any(|e| e.id == t.id) {
                list.push(IndexEntry {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    file: format!("{}.png", t.id),
                    spacing: t.spacing,
                    size: t.size,
                });
            }
        }
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = std::fs::write(dir.join("index.json"), json);
        }
    }
}

/// Light ink on transparent, for the dark theme.
fn thumb_image(gray: &[u8]) -> egui::ColorImage {
    let side = (gray.len() as f32).sqrt() as usize;
    let mut rgba = Vec::with_capacity(gray.len() * 4);
    for &g in gray {
        rgba.extend_from_slice(&[TEXT.r(), TEXT.g(), TEXT.b(), g]);
    }
    egui::ColorImage::from_rgba_unmultiplied([side, side], &rgba)
}

/// Shape dynamics as saved in a brush preset (prefs.json).
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct PresetDynamics {
    pub size_jitter: f32,
    pub min_diameter: f32,
    pub angle_jitter: f32,
    pub follow_direction: bool,
    pub roundness_jitter: f32,
    pub min_roundness: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    pub scatter_across: bool,
    pub count: u32,
    pub count_jitter: f32,
    pub opacity_jitter: f32,
    pub flow_jitter: f32,
    pub texture_depth: f32,
    pub texture_scale: f32,
    pub fg_bg_jitter: f32,
    pub hue_jitter: f32,
    pub saturation_jitter: f32,
    pub brightness_jitter: f32,
}

impl From<BrushDynamics> for PresetDynamics {
    fn from(d: BrushDynamics) -> Self {
        PresetDynamics {
            size_jitter: d.size_jitter,
            min_diameter: d.min_diameter,
            angle_jitter: d.angle_jitter,
            follow_direction: d.follow_direction,
            roundness_jitter: d.roundness_jitter,
            min_roundness: d.min_roundness,
            flip_x: d.flip_x_jitter,
            flip_y: d.flip_y_jitter,
            scatter_across: d.scatter_across,
            count: d.count,
            count_jitter: d.count_jitter,
            opacity_jitter: d.opacity_jitter,
            flow_jitter: d.flow_jitter,
            texture_depth: d.texture_depth,
            texture_scale: d.texture_scale,
            fg_bg_jitter: d.fg_bg_jitter,
            hue_jitter: d.hue_jitter,
            saturation_jitter: d.saturation_jitter,
            brightness_jitter: d.brightness_jitter,
        }
    }
}

impl From<&PresetDynamics> for BrushDynamics {
    fn from(p: &PresetDynamics) -> Self {
        let unit = |v: f32| v.clamp(0.0, 1.0);
        BrushDynamics {
            size_jitter: unit(p.size_jitter),
            min_diameter: unit(p.min_diameter),
            angle_jitter: unit(p.angle_jitter),
            follow_direction: p.follow_direction,
            roundness_jitter: unit(p.roundness_jitter),
            min_roundness: p.min_roundness.clamp(0.01, 1.0),
            flip_x_jitter: p.flip_x,
            flip_y_jitter: p.flip_y,
            scatter_across: p.scatter_across,
            count: p.count.clamp(1, 16),
            count_jitter: unit(p.count_jitter),
            opacity_jitter: unit(p.opacity_jitter),
            flow_jitter: unit(p.flow_jitter),
            texture_depth: unit(p.texture_depth),
            texture_scale: p.texture_scale.clamp(0.25, 4.0),
            fg_bg_jitter: unit(p.fg_bg_jitter),
            hue_jitter: unit(p.hue_jitter),
            saturation_jitter: unit(p.saturation_jitter),
            brightness_jitter: unit(p.brightness_jitter),
            background: BrushDynamics::default().background,
        }
    }
}

impl Default for PresetDynamics {
    fn default() -> Self {
        BrushDynamics::default().into()
    }
}

/// The popup id of the Brush settings panel.
pub(crate) fn panel_id() -> egui::Id {
    egui::Id::new("brush-settings")
}

impl App {
    /// A preset of the current brush (shape, tip, dynamics; not colour).
    pub(crate) fn current_preset(&self, name: String) -> session::BrushPreset {
        session::BrushPreset {
            name,
            radius: self.brush.radius,
            hardness: self.brush.hardness,
            spacing: self.brush.spacing,
            jitter: self.brush.jitter,
            opacity: self.brush.color[3],
            tip: self.brushes.selected.clone(),
            angle: self.brush.angle,
            roundness: self.brush.roundness,
            dynamics: self.brush.dynamics.into(),
        }
    }

    /// Put a preset on the brush. A tip that is gone falls back to round.
    pub(crate) fn apply_preset(&mut self, p: &session::BrushPreset) {
        self.select_tip(&p.tip);
        self.brush.radius = p.radius.clamp(0.5, 500.0);
        self.brush.hardness = p.hardness.clamp(0.0, 1.0);
        self.brush.spacing = p.spacing.clamp(0.02, 2.0);
        self.brush.jitter = p.jitter.clamp(0.0, MAX_SCATTER);
        self.brush.color[3] = p.opacity.clamp(0.0, 1.0);
        self.brush.angle = p.angle.clamp(-180.0, 180.0);
        self.brush.roundness = p.roundness.clamp(0.01, 1.0);
        self.brush.dynamics = (&p.dynamics).into();
    }

    /// Put back the brush from the last session (see `remember_brush`).
    pub(crate) fn restore_brush(&mut self) {
        if let Some(p) = self.prefs.current_brush.clone() {
            // A tip that has gone since is no news at launch.
            let status = std::mem::take(&mut self.status);
            self.apply_preset(&p);
            self.status = status;
        }
    }

    /// Keep the brush (tip, shape, dynamics) for the next session.
    pub(crate) fn remember_brush(&mut self) {
        self.prefs.current_brush = Some(Box::new(self.current_preset("Last used".into())));
        self.prefs.save();
    }

    /// Put tip `id` on the brush ("" = round). Picking a tip also picks
    /// its spacing, and an imported tip its own size.
    pub(crate) fn select_tip(&mut self, id: &str) {
        if id.is_empty() {
            if self.brush.tip.take().is_some() {
                self.brush.spacing = 0.12;
            }
            self.brushes.selected.clear();
            return;
        }
        let Some(i) = self.brushes.index_of(id) else {
            self.status = "That brush tip is no longer installed; using the round tip".into();
            self.select_tip("");
            return;
        };
        let Some(tip) = self.brushes.tip(i) else {
            self.status = format!("Could not load the brush tip \"{}\"", self.brushes.tips[i].name);
            return;
        };
        self.brush.tip = Some(tip);
        let e = &self.brushes.tips[i];
        if let Some(s) = e.spacing {
            self.brush.spacing = s.clamp(0.02, 2.0);
        }
        if let Some(size) = e.size {
            self.brush.radius = (size / 2.0).clamp(1.0, 200.0);
        }
        self.brushes.selected = id.to_string();
    }

    /// File ▸ Import brushes…
    pub(crate) fn pick_import_brushes(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Import brushes")
            .add_filter("Photoshop brushes", &["abr", "ABR"])
            .pick_file()
        {
            self.import_brushes(&p);
        }
    }

    /// Read a Photoshop brush set: sampled tips join the picker (and the
    /// `brushes` folder), computed round brushes become presets. The
    /// first new tip goes on the brush.
    pub(crate) fn import_brushes(&mut self, path: &Path) {
        let t0 = std::time::Instant::now();
        let file = file_name(&path.to_string_lossy());
        let set = match lumenply_io::abr::load(path) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Could not import {file}: {e}");
                return;
            }
        };
        if brushes_dir().is_none() {
            self.status = "Could not import brushes: no folder to keep them in".into();
            return;
        }
        let (mut tips, mut presets, mut known) = (0, 0, 0);
        let mut failed = set.warnings.len();
        let mut first = None;
        for b in set.brushes {
            match b.shape {
                AbrShape::Sampled {
                    width, height, gray, ..
                } => {
                    // ABR spacing is a fraction of the diameter; the size is
                    // the preset's own when the set gives one.
                    let spacing = b.spacing.map_or(0.5, |s| (s * 2.0).clamp(0.02, 2.0));
                    let size = b.diameter.unwrap_or(width.max(height) as f32);
                    match self.install_tip("abr", b.name, width, height, gray, spacing, size) {
                        Ok((id, new)) => {
                            if new {
                                tips += 1;
                            } else {
                                known += 1;
                            }
                            first.get_or_insert(id);
                        }
                        Err(_) => failed += 1,
                    }
                }
                AbrShape::Computed {
                    diameter,
                    hardness,
                    angle,
                    roundness,
                } => {
                    let p = session::BrushPreset {
                        name: b.name,
                        radius: (diameter / 2.0).clamp(0.5, 500.0),
                        hardness,
                        spacing: b.spacing.map_or(0.5, |s| (s * 2.0).clamp(0.02, 2.0)),
                        angle,
                        roundness,
                        ..session::BrushPreset::default()
                    };
                    if !self.prefs.brush_presets.contains(&p) {
                        self.prefs.brush_presets.push(p);
                        presets += 1;
                    }
                }
            }
        }
        if tips > 0 {
            self.brushes.save_index();
        }
        if presets > 0 {
            self.prefs.save();
        }
        if let Some(id) = first {
            self.select_tip(&id);
        }
        let mut parts = vec![format!("{tips} new tip{}", if tips == 1 { "" } else { "s" })];
        if presets > 0 {
            parts.push(format!("{presets} preset{}", if presets == 1 { "" } else { "s" }));
        }
        if known > 0 {
            parts.push(format!("{known} already installed"));
        }
        if failed > 0 {
            parts.push(format!("{failed} skipped"));
        }
        self.status = format!(
            "Imported {} from {file} in {:.2} s",
            parts.join(", "),
            t0.elapsed().as_secs_f32()
        );
    }

    /// Save a tip (16-bit coverage) into the brushes folder and the
    /// picker under `<prefix>-<content hash>`; the caller saves the index.
    /// Returns its id and whether it is new (an identical tip already
    /// installed is reused), or why it couldn't be kept.
    #[allow(clippy::too_many_arguments)]
    fn install_tip(
        &mut self,
        prefix: &str,
        name: String,
        width: u32,
        height: u32,
        gray: Vec<u16>,
        spacing: f32,
        size: f32,
    ) -> Result<(String, bool), String> {
        let id = format!("{prefix}-{:016x}", tip_hash(width, height, &gray));
        if self.brushes.index_of(&id).is_some() {
            return Ok((id, false));
        }
        let dir = brushes_dir().ok_or("no folder to keep brush tips in")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tip = BrushTip::from_gray16(name.clone(), width, height, &gray)?;
        let png = dir.join(format!("{id}.png"));
        image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(width, height, gray)
            .ok_or("the tip's size and pixels disagree")?
            .save(&png)
            .map_err(|e| e.to_string())?;
        let mut e = TipEntry::new(id.clone(), name);
        e.spacing = Some(spacing);
        e.size = Some(size);
        e.file = Some(png);
        e.thumb_gray = Some(tip.thumbnail(THUMB));
        self.brushes.tips.push(e);
        Ok((id, true))
    }

    /// Edit ▸ Define brush tip, as Photoshop's Define Brush Preset: the
    /// visible image inside the selection (or the whole canvas) becomes a
    /// sampled tip — dark pixels paint, light and transparent ones don't —
    /// trimmed to what it covers, and goes on the brush.
    pub(crate) fn define_brush_tip(&mut self) {
        const MAX_SIDE: u32 = 2500;
        let doc = self.editor.doc();
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let area = sel.as_ref().map_or(canvas, |s| s.tight_bounds(canvas));
        if area.is_empty() {
            self.status = "Select the part of the image to make a tip from".into();
            return;
        }
        // Only the selected area is composited.
        let flat = lumenply_render::composite_rect(doc, area).to_raster(area);
        let enc = lumenply_doc::adjust::srgb_encode;
        let (w, h) = (area.w as usize, area.h as usize);
        let mut cov = vec![0f32; w * h];
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                let (cx, cy) = (area.x + x as i32, area.y + y as i32);
                let [r, g, b, a] = flat.get(x as u32, y as u32).to_straight();
                let luma = 0.2126 * enc(r) + 0.7152 * enc(g) + 0.0722 * enc(b);
                let c = ((1.0 - luma) * a * sel.as_ref().map_or(1.0, |s| s.value(cx, cy))).clamp(0.0, 1.0);
                cov[y * w + x] = c;
                if c >= 1.0 / 255.0 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
        if x1 <= x0 || y1 <= y0 {
            self.status = "Nothing to make a tip from: the selection is light or empty".into();
            return;
        }
        let (tw, th) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if tw > MAX_SIDE || th > MAX_SIDE {
            self.status = format!("A brush tip can be at most {MAX_SIDE} px on a side; this is {tw}×{th}");
            return;
        }
        let mut gray = Vec::with_capacity((tw * th) as usize);
        for y in y0..y1 {
            for x in x0..x1 {
                gray.push((cov[y * w + x] * 65535.0 + 0.5) as u16);
            }
        }
        let n = self
            .brushes
            .tips
            .iter()
            .filter(|t| t.id.starts_with("custom-"))
            .count()
            + 1;
        let name = format!("Custom tip {n}");
        match self.install_tip("custom", name, tw, th, gray, 0.5, tw.max(th) as f32) {
            Ok((id, _)) => {
                self.brushes.save_index();
                self.select_tip(&id);
                let name = self.brush.tip.as_ref().map_or("", |t| t.name()).to_string();
                self.status = format!("Defined the brush tip \"{name}\" ({tw}×{th} px)");
            }
            Err(e) => self.status = format!("Could not define a brush tip: {e}"),
        }
    }

    /// Remove an imported tip from the picker and the brushes folder.
    fn delete_tip(&mut self, id: &str) {
        let Some(i) = self.brushes.index_of(id).filter(|_| !id.starts_with("builtin:")) else {
            return;
        };
        let e = self.brushes.tips.remove(i);
        self.brushes.removed.push(e.id.clone());
        if let Some(f) = e.file {
            let _ = std::fs::remove_file(f);
        }
        self.brushes.save_index();
        if self.brushes.selected == id {
            self.select_tip("");
        }
        self.status = format!("Removed the brush tip \"{}\"", e.name);
    }

    /// The options-bar button showing the current tip; it opens the
    /// Brush settings panel.
    pub(crate) fn brush_settings_button(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let open = ctx.memory(|m| m.is_popup_open(panel_id()));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(44.0, 26.0), Sense::click());
        let dynamic = !self.brush.dynamics.is_static() || self.brush.jitter > 0.0;
        let name = if dynamic {
            "Brush settings (dynamics on)"
        } else {
            "Brush settings"
        };
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, open, name));
        theme::note_target(&ctx, "brush-settings", rect);
        let fill = if open {
            ACCENT_TINT
        } else if resp.hovered() {
            HOVER
        } else {
            CONTROL
        };
        ui.painter().rect_filled(rect, RADIUS, fill);
        if open {
            ui.painter().rect_stroke(rect, RADIUS, Stroke::new(1.0, ACCENT));
        }
        let tip_rect =
            egui::Rect::from_center_size(rect.left_center() + egui::vec2(14.0, 0.0), egui::vec2(20.0, 20.0));
        self.paint_tip(ui, tip_rect, self.brushes.selected.clone().as_str());
        let caret = rect.right_center() - egui::vec2(10.0, 0.0);
        let p = ui.painter();
        p.add(Shape::convex_polygon(
            vec![
                caret + egui::vec2(-4.0, -2.0),
                caret + egui::vec2(4.0, -2.0),
                caret + egui::vec2(0.0, 3.0),
            ],
            MUTED,
            Stroke::NONE,
        ));
        // A dot while dynamics vary the dabs, so a jittery brush is no
        // surprise.
        if dynamic {
            p.circle_filled(rect.right_top() + egui::vec2(-4.0, 4.0), 2.5, ACCENT);
        }
        focus_ring(ui, &resp, rect, RADIUS);
        let resp = resp.on_hover_text("Brush settings: tip, shape dynamics, scattering and transfer");
        if resp.clicked() {
            ctx.memory_mut(|m| m.toggle_popup(panel_id()));
        }
        if ctx.memory(|m| m.is_popup_open(panel_id())) {
            self.brush_settings_panel(&ctx, rect);
        }
    }

    /// Paint tip `id`'s picture into `rect`.
    fn paint_tip(&mut self, ui: &egui::Ui, rect: egui::Rect, id: &str) {
        if id.is_empty() {
            let r = rect.width() * 0.32;
            let soft = Color32::from_rgba_unmultiplied(TEXT.r(), TEXT.g(), TEXT.b(), 90);
            ui.painter().circle_filled(rect.center(), r + 2.0, soft);
            ui.painter().circle_filled(rect.center(), r, TEXT);
            return;
        }
        let tex = self
            .brushes
            .index_of(id)
            .and_then(|i| self.brushes.thumb(ui.ctx(), i));
        match tex {
            Some(t) => {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(t.id(), rect, uv, Color32::WHITE);
            }
            None => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "?",
                    FontId::proportional(14.0),
                    MUTED,
                );
            }
        }
    }

    /// One cell of the tip grid; true when clicked.
    fn tip_cell(&mut self, ui: &mut egui::Ui, id: &str, name: &str) -> bool {
        let selected = self.brushes.selected == id;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(CELL, CELL), Sense::click());
        let label = format!("Brush tip {name}");
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
        });
        let fill = if selected {
            ACCENT_TINT
        } else if resp.hovered() {
            HOVER
        } else {
            RAISED
        };
        ui.painter().rect_filled(rect, 5.0, fill);
        if selected {
            ui.painter().rect_stroke(rect, 5.0, Stroke::new(1.0, ACCENT));
        }
        self.paint_tip(ui, rect.shrink(4.0), id);
        focus_ring(ui, &resp, rect, 5.0);
        resp.on_hover_text(name).clicked()
    }

    /// The floating Brush settings panel, under the options-bar button at
    /// `anchor`. It stays open while painting (so dynamics can be tried
    /// out) until its button or its close button is pressed.
    fn brush_settings_panel(&mut self, ctx: &egui::Context, anchor: egui::Rect) {
        let screen = ctx.screen_rect();
        let mut close = false;
        // Short windows scroll the settings rather than cover the bar.
        let room = (screen.bottom() - anchor.bottom() - 8.0 - 12.0 - 24.0 - 40.0).max(160.0);
        // As tall as the settings were last frame, so a roomy window shows
        // them whole without scrolling.
        let content_id = panel_id().with("content-h");
        let content_h = ctx.data(|d| d.get_temp::<f32>(content_id)).unwrap_or(room);
        let room = room.min(content_h + 2.0);
        egui::Area::new(panel_id())
            .kind(egui::UiKind::Popup)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(anchor.left(), anchor.bottom() + 8.0))
            .fade_in(false)
            .sense(BACKDROP_SENSE)
            .constrain_to(screen.shrink(4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Brush settings").strong().color(TEXT));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let r = ui.small_button("×").on_hover_text("Close brush settings");
                                a11y_name(&r, "Close brush settings");
                                close = r.clicked();
                            });
                        });
                        let out = egui::ScrollArea::vertical()
                            .id_salt("brush-settings-scroll")
                            .max_height(room)
                            .min_scrolled_height(room)
                            .show(ui, |ui| {
                                ui.horizontal_top(|ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(COL_W);
                                        self.tip_column(ui);
                                    });
                                    ui.add_space(12.0);
                                    ui.separator();
                                    ui.add_space(4.0);
                                    ui.vertical(|ui| {
                                        ui.set_width(COL_W);
                                        self.dynamics_column(ui);
                                    });
                                });
                            });
                        a11y_scroll(ui.ctx(), &out, "Brush settings");
                        let h = out.content_size.y;
                        ui.ctx().data_mut(|d| d.insert_temp(content_id, h));
                    });
            });
        if close {
            ctx.memory_mut(|m| m.close_popup());
        }
    }

    fn tip_column(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "TIP");
        let mut cells: Vec<(String, String)> = vec![(String::new(), "Round".into())];
        cells.extend(self.brushes.tips.iter().map(|t| (t.id.clone(), t.name.clone())));
        let rows = cells.len().div_ceil(PER_ROW);
        let mut pick = None;
        let out = egui::ScrollArea::vertical()
            .id_salt("brush-tip-grid")
            .max_height(3.0 * (CELL + 4.0))
            .auto_shrink([false, true])
            .show_rows(ui, CELL, rows, |ui, range| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for r in range {
                    ui.horizontal(|ui| {
                        for (id, name) in cells.iter().skip(r * PER_ROW).take(PER_ROW) {
                            if self.tip_cell(ui, id, name) {
                                pick = Some(id.clone());
                            }
                        }
                    });
                }
            });
        a11y_scroll(ui.ctx(), &out, "Brush tips");
        if let Some(id) = pick {
            self.select_tip(&id);
        }
        let about = match &self.brush.tip {
            Some(t) => format!("{} · {}×{} px", t.name(), t.width(), t.height()),
            None => "Round · hardness from the bar".into(),
        };
        ui.label(RichText::new(about).small().color(MUTED));
        ui.horizontal(|ui| {
            if ui
                .button("Import .abr…")
                .on_hover_text("Add the tips of a Photoshop brush set (.abr)")
                .clicked()
            {
                self.pick_import_brushes();
            }
            let imported =
                !self.brushes.selected.is_empty() && !self.brushes.selected.starts_with("builtin:");
            if ui
                .add_enabled(imported, egui::Button::new("Remove tip"))
                .on_hover_text("Remove this imported tip from the list")
                .on_disabled_hover_text("Only imported tips can be removed")
                .clicked()
            {
                let id = self.brushes.selected.clone();
                self.delete_tip(&id);
            }
        });
        section_title(ui, "TIP SHAPE");
        slider_row_ex(
            ui,
            "Angle",
            &mut self.brush.angle,
            -180.0..=180.0,
            "°",
            RowOpts {
                label_w: LABEL,
                ..RowOpts::default()
            },
        );
        slider_row_scaled_w(
            ui,
            "Roundness",
            &mut self.brush.roundness,
            0.01..=1.0,
            100.0,
            "%",
            LABEL,
        );
        // Shown as Photoshop does, in percent of the diameter.
        let mut spacing = self.brush.spacing * 0.5;
        let before = spacing;
        slider_row_scaled_w(ui, "Spacing", &mut spacing, 0.01..=1.0, 100.0, "%", LABEL);
        if spacing != before {
            self.brush.spacing = spacing * 2.0;
        }
        section_title(ui, "TEXTURE");
        let d = &mut self.brush.dynamics;
        slider_row_scaled_w(
            ui,
            "Grain depth",
            &mut d.texture_depth,
            0.0..=1.0,
            100.0,
            "%",
            LABEL,
        );
        slider_row_scaled_w(
            ui,
            "Grain scale",
            &mut d.texture_scale,
            0.25..=4.0,
            100.0,
            "%",
            LABEL,
        );
        ui.label(
            RichText::new("Paper tooth the paint can't fill, fixed to the canvas")
                .small()
                .color(MUTED),
        );
        section_title(ui, "COLOUR DYNAMICS");
        let d = &mut self.brush.dynamics;
        for (label, v) in [
            ("Fg/bg jitter", &mut d.fg_bg_jitter),
            ("Hue jitter", &mut d.hue_jitter),
            ("Saturation jitter", &mut d.saturation_jitter),
            ("Brightness jitter", &mut d.brightness_jitter),
        ] {
            slider_row_scaled_w(ui, label, v, 0.0..=1.0, 100.0, "%", LABEL);
        }
    }

    fn dynamics_column(&mut self, ui: &mut egui::Ui) {
        let d = &mut self.brush.dynamics;
        let pct = |ui: &mut egui::Ui, label: &str, v: &mut f32| {
            slider_row_scaled_w(ui, label, v, 0.0..=1.0, 100.0, "%", LABEL);
        };
        section_title(ui, "SHAPE DYNAMICS");
        pct(ui, "Size jitter", &mut d.size_jitter);
        pct(ui, "Minimum diameter", &mut d.min_diameter);
        pct(ui, "Angle jitter", &mut d.angle_jitter);
        check(ui, &mut d.follow_direction, "Angle follows the stroke direction");
        pct(ui, "Roundness jitter", &mut d.roundness_jitter);
        slider_row_scaled_w(
            ui,
            "Minimum roundness",
            &mut d.min_roundness,
            0.01..=1.0,
            100.0,
            "%",
            LABEL,
        );
        ui.horizontal(|ui| {
            check(ui, &mut d.flip_x_jitter, "Flip X jitter");
            check(ui, &mut d.flip_y_jitter, "Flip Y jitter");
        });
        section_title(ui, "SCATTERING");
        slider_row_scaled_w(
            ui,
            "Scatter",
            &mut self.brush.jitter,
            0.0..=MAX_SCATTER,
            100.0,
            "%",
            LABEL,
        );
        let mut both = !d.scatter_across;
        if check(ui, &mut both, "Both axes")
            .on_hover_text("Off: dabs scatter only across the stroke")
            .changed()
        {
            d.scatter_across = !both;
        }
        let mut count = d.count as f32;
        slider_row_ex(
            ui,
            "Count",
            &mut count,
            1.0..=16.0,
            "",
            RowOpts {
                label_w: LABEL,
                int: true,
                ..RowOpts::default()
            },
        );
        d.count = count.round().clamp(1.0, 16.0) as u32;
        pct(ui, "Count jitter", &mut d.count_jitter);
        section_title(ui, "TRANSFER");
        pct(ui, "Opacity jitter", &mut d.opacity_jitter);
        pct(ui, "Flow jitter", &mut d.flow_jitter);
        ui.add_space(4.0);
        if ui
            .button("Reset dynamics")
            .on_hover_text("No jitter, scatter, count or grain; angle 0° and full roundness")
            .clicked()
        {
            self.brush.dynamics = BrushDynamics::default();
            self.brush.jitter = 0.0;
            self.brush.angle = 0.0;
            self.brush.roundness = 1.0;
        }
    }

    /// The brush cursor in the tip's shape at screen point `p`: a sampled
    /// tip as a faint print of itself, a squashed round tip as its turned
    /// ellipse. False (draw the plain circle) for the round tip or when
    /// the cursor would be too small to read.
    pub(crate) fn tip_cursor(&self, painter: &egui::Painter, p: Pos2) -> bool {
        let r = self.brush.radius * self.zoom;
        if r < 4.0 {
            return false;
        }
        let rho = self.brush.roundness.clamp(0.01, 1.0);
        let rot = egui::emath::Rot2::from_angle(-self.brush.angle.to_radians());
        if self.brush.tip.is_none() {
            if rho >= 1.0 {
                return false;
            }
            let pts: Vec<Pos2> = (0..48)
                .map(|i| {
                    let a = i as f32 / 48.0 * std::f32::consts::TAU;
                    p + rot * egui::vec2(a.cos() * r, a.sin() * r * rho)
                })
                .collect();
            painter.add(Shape::closed_line(
                pts.clone(),
                Stroke::new(3.0, Color32::from_black_alpha(140)),
            ));
            painter.add(Shape::closed_line(pts, Stroke::new(1.0, Color32::WHITE)));
            return true;
        }
        let tex = self
            .brushes
            .index_of(&self.brushes.selected)
            .and_then(|i| self.brushes.tips[i].thumb.clone());
        let Some(tex) = tex else {
            return false;
        };
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let rect = egui::Rect::from_center_size(p, egui::vec2(2.0 * r, 2.0 * r * rho));
        for (offset, tint) in [
            (egui::vec2(1.0, 1.0), Color32::from_black_alpha(110)),
            (egui::Vec2::ZERO, Color32::from_white_alpha(150)),
        ] {
            let mut mesh = egui::Mesh::with_texture(tex.id());
            mesh.add_rect_with_uv(rect.translate(offset), uv, tint);
            mesh.rotate(rot, p + offset);
            painter.add(Shape::mesh(mesh));
        }
        // A crosshair marks the hot spot inside the print.
        let s = Stroke::new(1.0, Color32::WHITE);
        painter.line_segment([p - egui::vec2(4.0, 0.0), p + egui::vec2(4.0, 0.0)], s);
        painter.line_segment([p - egui::vec2(0.0, 4.0), p + egui::vec2(0.0, 4.0)], s);
        true
    }

    /// Brush tokens for `--screenshot-do` (`brush:...`): `brush:panel`
    /// opens Brush settings; `brush:tip=Name` (or `Round`, or `#n` for
    /// the n-th tip) picks a tip; `brush:set=key:value` sets size, angle,
    /// roundness, spacing, hardness, opacity, scatter (percent values),
    /// size-jitter, min-diameter, angle-jitter, roundness-jitter, count,
    /// count-jitter, opacity-jitter, flow-jitter, follow, across, flip
    /// (0/1); `brush:layer` adds an empty layer to paint on;
    /// `brush:color=RRGGBB`; `brush:stroke=X0:Y0:X1:Y1[:WAVE]` paints a
    /// straight (or sine-wave) stroke in document pixels with the current
    /// tool, timing it; `brush:import=PATH` imports an .abr.
    pub(crate) fn debug_brush(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("brush:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        match verb {
            "panel" => ctx.memory_mut(|m| m.open_popup(panel_id())),
            "tip" => {
                let name = arg.replace('_', " ");
                let id = if name.eq_ignore_ascii_case("round") {
                    Some(String::new())
                } else if let Some(n) = name.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
                    self.brushes.tips.get(n.saturating_sub(1)).map(|t| t.id.clone())
                } else {
                    self.brushes
                        .tips
                        .iter()
                        .find(|t| t.name.eq_ignore_ascii_case(&name))
                        .map(|t| t.id.clone())
                };
                match id {
                    Some(id) => self.select_tip(&id),
                    None => eprintln!("brush: no tip named {name:?}"),
                }
            }
            "set" => {
                let Some((k, v)) = arg.split_once(':') else {
                    return false;
                };
                let Ok(v) = v.parse::<f32>() else {
                    return false;
                };
                let b = &mut self.brush;
                let d = &mut b.dynamics;
                match k {
                    "size" => b.radius = v / 2.0,
                    "angle" => b.angle = v,
                    "roundness" => b.roundness = v / 100.0,
                    "spacing" => b.spacing = v / 50.0,
                    "hardness" => b.hardness = v / 100.0,
                    "opacity" => b.color[3] = v / 100.0,
                    "scatter" => b.jitter = v / 100.0,
                    "size-jitter" => d.size_jitter = v / 100.0,
                    "min-diameter" => d.min_diameter = v / 100.0,
                    "angle-jitter" => d.angle_jitter = v / 100.0,
                    "roundness-jitter" => d.roundness_jitter = v / 100.0,
                    "count" => d.count = v as u32,
                    "count-jitter" => d.count_jitter = v / 100.0,
                    "opacity-jitter" => d.opacity_jitter = v / 100.0,
                    "flow-jitter" => d.flow_jitter = v / 100.0,
                    "follow" => d.follow_direction = v != 0.0,
                    "across" => d.scatter_across = v != 0.0,
                    "flip" => d.flip_x_jitter = v != 0.0,
                    "grain" => d.texture_depth = v / 100.0,
                    "grain-scale" => d.texture_scale = v / 100.0,
                    "fgbg" => d.fg_bg_jitter = v / 100.0,
                    "hue-jitter" => d.hue_jitter = v / 100.0,
                    "sat-jitter" => d.saturation_jitter = v / 100.0,
                    "bri-jitter" => d.brightness_jitter = v / 100.0,
                    _ => return false,
                }
            }
            "layer" => self.add_pixel_layer(),
            "define" => self.define_brush_tip(),
            "color" => {
                let hex = u32::from_str_radix(arg.trim_start_matches('#'), 16).unwrap_or(0);
                self.brush_rgb = [
                    ((hex >> 16) & 255) as f32 / 255.0,
                    ((hex >> 8) & 255) as f32 / 255.0,
                    (hex & 255) as f32 / 255.0,
                ];
            }
            "stroke" => {
                let n: Vec<f32> = arg.split(':').filter_map(|s| s.parse().ok()).collect();
                if n.len() < 4 {
                    return false;
                }
                let (x0, y0, x1, y1) = (n[0], n[1], n[2], n[3]);
                let wave = n.get(4).copied().unwrap_or(0.0);
                let len = (x1 - x0).hypot(y1 - y0).max(1.0);
                let (nx, ny) = (-(y1 - y0) / len, (x1 - x0) / len);
                let steps = (len / 3.0).ceil() as usize;
                let points: Vec<StrokePoint> = (0..=steps)
                    .map(|i| {
                        let t = i as f32 / steps as f32;
                        let off = wave * (t * std::f32::consts::TAU * 1.5).sin();
                        StrokePoint::new(x0 + (x1 - x0) * t + nx * off, y0 + (y1 - y0) * t + ny * off, 1.0)
                    })
                    .collect();
                if let Some(layer) = self.active {
                    let cmd = self.stroke_command(layer, points);
                    let t = std::time::Instant::now();
                    self.run(cmd.as_ref());
                    eprintln!(
                        "brush: {} stroke {len:.0} px long in {:.1} ms",
                        self.brushes
                            .index_of(&self.brushes.selected)
                            .map_or("Round", |i| self.brushes.tips[i].name.as_str()),
                        t.elapsed().as_secs_f64() * 1000.0
                    );
                }
            }
            "import" => {
                self.import_brushes(Path::new(arg));
                eprintln!("{}", self.status);
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    fn be32(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    /// A version-1 brush set: one computed brush, one 3×2 sampled tip
    /// whose pixels carry `seed` (so each test run imports its own tip).
    fn abr_v1(seed: u8) -> Vec<u8> {
        let mut computed = be32(0).to_vec();
        for x in [25u16, 30, 60, 45, 70] {
            computed.extend(be16(x));
        }
        let mut sampled = be32(0).to_vec();
        sampled.extend(be16(50));
        sampled.push(1);
        sampled.extend([0u8; 8]);
        for x in [0u32, 0, 2, 3] {
            sampled.extend(be32(x));
        }
        sampled.extend(be16(8));
        sampled.push(0);
        sampled.extend([255, seed, 0, 128, 255, 64]);
        let mut file = be16(1).to_vec();
        file.extend(be16(2));
        for (kind, body) in [(1u16, &computed), (2, &sampled)] {
            file.extend(be16(kind));
            file.extend(be32(body.len() as u32));
            file.extend(body);
        }
        file
    }

    fn launch() -> App {
        let mut app = App::launch(&[]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app
    }

    #[test]
    fn importing_an_abr_adds_its_tip_and_presets_and_picks_the_tip() {
        let mut app = launch();
        let seed = (std::process::id() % 251) as u8;
        let path = std::env::temp_dir().join(format!("lumenply-test-{}.abr", std::process::id()));
        std::fs::write(&path, abr_v1(seed)).unwrap();
        app.import_brushes(&path);
        let _ = std::fs::remove_file(&path);
        assert!(app.status.starts_with("Imported "), "{}", app.status);
        let tip = app.brush.tip.clone().expect("the imported tip is on the brush");
        assert_eq!((tip.width(), tip.height()), (3, 2));
        assert_eq!(tip.coverage(0, 0), 1.0);
        assert_eq!(tip.coverage(1, 0), seed as f32 / 255.0);
        assert_eq!(tip.coverage(2, 1), 64.0 / 255.0);
        // ABR spacing 50 % of the diameter = 1.0 of the radius; the size
        // is the tip's own (3 px wide).
        assert_eq!(app.brush.spacing, 1.0);
        assert_eq!(app.brush.radius, 1.5);
        let preset = app
            .prefs
            .brush_presets
            .iter()
            .find(|p| p.name == "Round 30")
            .expect("the computed brush became a preset");
        assert_eq!(
            (
                preset.radius,
                preset.hardness,
                preset.angle,
                preset.roundness,
                preset.spacing
            ),
            (15.0, 0.7, 45.0, 0.6, 0.5)
        );
        // The tip survives a restart: the next launch lists it.
        let id = app.brushes.selected.clone();
        assert!(id.starts_with("abr-"));
        let mut again = launch();
        let i = again.brushes.index_of(&id).expect("listed after restart");
        assert_eq!(again.brushes.tip(i).unwrap().coverage(2, 1), 64.0 / 255.0);
        // A preset remembers the tip and dynamics.
        app.brush.dynamics.size_jitter = 0.5;
        app.brush.angle = 30.0;
        let p = app.current_preset("Mine".into());
        again.apply_preset(&p);
        assert_eq!(again.brushes.selected, id);
        assert_eq!(again.brush.dynamics.size_jitter, 0.5);
        assert_eq!(again.brush.angle, 30.0);
        // Back to round restores the default spacing.
        again.select_tip("");
        assert!(again.brush.tip.is_none());
        assert_eq!(again.brush.spacing, 0.12);
        // Removing it deletes it from the list.
        app.delete_tip(&id);
        assert!(app.brushes.index_of(&id).is_none() && app.brush.tip.is_none());
    }

    #[test]
    fn the_last_brush_comes_back_at_launch() {
        let mut app = launch();
        app.select_tip("builtin:Leaf");
        app.brush.radius = 33.0;
        app.brush.angle = -40.0;
        app.brush.dynamics.scatter_across = true;
        app.brush.dynamics.texture_depth = 0.4;
        // What `remember_brush` writes at exit (without touching the
        // shared test prefs file).
        let saved = app.current_preset("Last used".into());
        let mut next = launch();
        next.prefs.current_brush = Some(Box::new(saved));
        next.status = "Ready".into();
        next.restore_brush();
        assert_eq!(next.brushes.selected, "builtin:Leaf");
        assert_eq!(next.brush.tip.as_ref().map(|t| t.name()), Some("Leaf"));
        assert_eq!((next.brush.radius, next.brush.angle), (33.0, -40.0));
        assert!(next.brush.dynamics.scatter_across);
        assert_eq!(next.brush.dynamics.texture_depth, 0.4);
        assert_eq!(next.status, "Ready");
    }

    #[test]
    fn define_brush_tip_turns_dark_pixels_in_the_selection_into_a_tip() {
        let mut app = launch();
        app.open_in_new_tab(blank(64, 64), None);
        let layer = app.editor.doc().layers()[0].id;
        // A black 6×4 block on white, a grey pixel beside it.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(20, 30, 6, 4))),
        });
        app.run(&Fill {
            layer,
            color: [0.0, 0.0, 0.0, 1.0],
        });
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(26, 30, 1, 1))),
        });
        // sRGB mid grey: luminance 0.5 encoded, so half coverage.
        let mid = lumenply_doc::adjust::srgb_decode(0.5);
        app.run(&Fill {
            layer,
            color: [mid, mid, mid, 1.0],
        });
        // Select generously around both: the white margin is trimmed.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 20, 30, 30))),
        });
        app.define_brush_tip();
        let tip = app.brush.tip.clone().expect("the new tip is on the brush");
        assert_eq!((tip.width(), tip.height()), (7, 4), "{}", app.status);
        assert_eq!(tip.coverage(0, 0), 1.0);
        assert_eq!(tip.coverage(5, 3), 1.0);
        assert!((tip.coverage(6, 0) - 0.5).abs() < 0.01, "{}", tip.coverage(6, 0));
        assert_eq!(tip.coverage(6, 1), 0.0);
        assert!(app.brushes.selected.starts_with("custom-"));
        assert!(app.status.starts_with("Defined the brush tip"), "{}", app.status);
        // The tip's own size and half-diameter spacing come with it.
        assert_eq!((app.brush.radius, app.brush.spacing), (3.5, 0.5));
        // A white-only selection has nothing to paint with.
        app.run(&SetSelection {
            selection: Some(Selection::rect(Rect::new(40, 40, 8, 8))),
        });
        app.define_brush_tip();
        assert!(
            app.status.starts_with("Nothing to make a tip from"),
            "{}",
            app.status
        );
        let id = app.brushes.selected.clone();
        app.delete_tip(&id);
    }

    #[test]
    fn a_broken_abr_reports_why() {
        let mut app = launch();
        let path = std::env::temp_dir().join(format!("lumenply-bad-{}.abr", std::process::id()));
        std::fs::write(&path, [0u8, 9, 0, 0]).unwrap();
        app.import_brushes(&path);
        let _ = std::fs::remove_file(&path);
        assert!(
            app.status.contains("version 9 brush sets are not supported"),
            "{}",
            app.status
        );
        assert!(app.brush.tip.is_none());
    }

    #[test]
    fn presets_from_older_prefs_load_with_round_defaults() {
        let p: session::BrushPreset = serde_json::from_str(
            r#"{"name":"Old","radius":4,"hardness":1,"spacing":0.2,"jitter":0,"opacity":1}"#,
        )
        .unwrap();
        assert_eq!((p.tip.as_str(), p.angle, p.roundness), ("", 0.0, 1.0));
        assert_eq!(BrushDynamics::from(&p.dynamics), BrushDynamics::default());
    }

    #[test]
    fn the_brush_settings_panel_names_every_control() {
        let mut app = launch();
        app.open_in_new_tab(blank(64, 64), None);
        app.tool = Tool::Brush;
        let ctx = crate::a11y_tests::ctx();
        ctx.memory_mut(|m| m.open_popup(panel_id()));
        let missing = crate::a11y_tests::nameless(&mut app, &ctx);
        assert!(
            ctx.memory(|m| m.is_popup_open(panel_id())),
            "the panel stayed open"
        );
        assert_eq!(missing, Vec::<String>::new());
        // Its tip cells are announced as selectable, with the tip's name.
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| app.frame(ctx));
        let names: Vec<String> = out
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .filter_map(|(_, n)| n.name().map(str::to_string))
            .collect();
        for want in [
            "Brush tip Round",
            "Brush tip Chalk",
            "Size jitter",
            "Count",
            "Brush settings",
        ] {
            assert!(names.iter().any(|n| n == want), "{want} missing");
        }
    }
}
