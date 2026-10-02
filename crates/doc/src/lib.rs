//! The document model: a canvas size and a tree of layers.
//!
//! Layers are ordered bottom-to-top. Pixel layers own a sparse
//! [`TileStore`]; group layers own child layers and composite them as an
//! isolated group. Adjustment, text, vector and smart-object layers will be
//! further [`LayerContent`] variants, and layer masks will hang off
//! [`Layer`], without changing this shape.
//!
//! `Document` is cheap to clone (tiles are shared), which is what the undo
//! history relies on.

use std::str::FromStr;

use lumenply_tiles::{Rect, Rgba, TileStore};
use serde::{Deserialize, Serialize};

pub type LayerId = u64;

/// How a layer combines with everything below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    Difference,
    Add,
    HardLight,
    SoftLight,
}

impl BlendMode {
    pub const ALL: [BlendMode; 10] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Add,
        BlendMode::HardLight,
        BlendMode::SoftLight,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::Difference => "difference",
            BlendMode::Add => "add",
            BlendMode::HardLight => "hard-light",
            BlendMode::SoftLight => "soft-light",
        }
    }
}

impl FromStr for BlendMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_lowercase().replace('_', "-");
        BlendMode::ALL
            .iter()
            .copied()
            .find(|m| m.name() == key)
            .ok_or_else(|| format!("unknown blend mode '{s}'"))
    }
}

pub mod adjust;
pub mod selection;

pub use adjust::{Adjustment, CompiledAdjustment, LevelsChannel};
pub use selection::{CombineOp, Selection};

/// A pixel filter: destructive when applied to a layer, live when it is a
/// [`LayerContent::Filter`] layer. Kernels live in `lumenply-render`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Filter {
    GaussianBlur {
        radius: f32,
    },
    BoxBlur {
        radius: f32,
    },
    /// Unsharp mask: `out = src + amount · (src − blur(src, radius))`.
    Sharpen {
        amount: f32,
        radius: f32,
    },
    /// Monochromatic uniform noise, `amount` in 0..=1, seeded by pixel
    /// position so tiles and re-renders agree.
    Noise {
        amount: f32,
    },
    /// Average along a straight line: `angle` in degrees, `distance` in
    /// pixels.
    MotionBlur {
        angle: f32,
        distance: f32,
    },
    /// Per-channel median in a square window (radius capped at 8 px).
    Median {
        radius: f32,
    },
    /// The detail above `radius`: mid grey plus source minus blur.
    HighPass {
        radius: f32,
    },
}

impl Filter {
    pub fn name(&self) -> &'static str {
        match self {
            Filter::GaussianBlur { .. } => "Gaussian Blur",
            Filter::BoxBlur { .. } => "Box Blur",
            Filter::Sharpen { .. } => "Sharpen",
            Filter::Noise { .. } => "Add Noise",
            Filter::MotionBlur { .. } => "Motion Blur",
            Filter::Median { .. } => "Median",
            Filter::HighPass { .. } => "High Pass",
        }
    }

    /// Median windows are gathered per pixel, so keep them small.
    pub fn median_radius(radius: f32) -> i32 {
        (sane_radius(radius).round() as i32).clamp(1, 8)
    }

    /// How far (in pixels) the filter reads outside the area it produces.
    /// Always non-negative, whatever the stored parameters say.
    pub fn pad(&self) -> i32 {
        match self {
            Filter::GaussianBlur { radius } | Filter::Sharpen { radius, .. } => box_radius(*radius) * 3,
            Filter::BoxBlur { radius } => sane_radius(*radius).round() as i32,
            Filter::Noise { .. } => 0,
            Filter::MotionBlur { distance, .. } => (sane_radius(*distance) / 2.0).ceil() as i32 + 1,
            Filter::Median { radius } => Filter::median_radius(*radius),
            Filter::HighPass { radius } => box_radius(*radius) * 3,
        }
    }
}

/// Clamp a user- or file-supplied blur radius to something the engine can
/// honour: finite and within [0, 1000]. NaN and infinities become 0, which
/// filters treat as a no-op; anything bigger than 1000 px is already far
/// beyond a visible difference and would only size absurd paddings.
pub fn sane_radius(radius: f32) -> f32 {
    if radius.is_finite() {
        radius.clamp(0.0, 1000.0)
    } else {
        0.0
    }
}

/// Box-blur radius whose triple pass approximates a Gaussian of `sigma_like`.
pub fn box_radius(sigma_like: f32) -> i32 {
    ((sane_radius(sigma_like) / 3f32.sqrt()).round() as i32).max(1)
}

/// Non-destructive per-layer effects ("layer styles"), rendered from the
/// layer's own coverage at composite time. Kernels live in `lumenply-render`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerEffects {
    pub drop_shadow: Option<ShadowFx>,
    pub outer_glow: Option<GlowFx>,
    /// Flat colour painted over the layer's coverage.
    pub color_overlay: Option<ColorOverlayFx>,
    /// Linear gradient painted over the layer's coverage.
    pub gradient_overlay: Option<GradientOverlayFx>,
    /// Shadow cast by the coverage edge onto the layer's inside.
    pub inner_shadow: Option<ShadowFx>,
    /// Glow creeping inward from the coverage edge.
    pub inner_glow: Option<GlowFx>,
    pub stroke: Option<StrokeFx>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorOverlayFx {
    /// Straight linear RGB.
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for ColorOverlayFx {
    fn default() -> Self {
        ColorOverlayFx {
            color: [1.0, 0.45, 0.1],
            opacity: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientOverlayFx {
    /// Straight linear RGB at the gradient's start and end.
    pub start: [f32; 3],
    pub end: [f32; 3],
    /// Direction in degrees: 0 runs left → right, 90 bottom → top.
    pub angle: f32,
    pub opacity: f32,
}

impl Default for GradientOverlayFx {
    fn default() -> Self {
        GradientOverlayFx {
            start: [0.1, 0.3, 0.9],
            end: [0.9, 0.2, 0.5],
            angle: 90.0,
            opacity: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShadowFx {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    /// Straight linear RGB.
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for ShadowFx {
    fn default() -> Self {
        ShadowFx {
            dx: 4.0,
            dy: 4.0,
            blur: 6.0,
            color: [0.0, 0.0, 0.0],
            opacity: 0.6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GlowFx {
    pub blur: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for GlowFx {
    fn default() -> Self {
        GlowFx {
            blur: 8.0,
            color: [1.0, 0.9, 0.4],
            opacity: 0.8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeFx {
    /// Outline width in pixels, grown outward from the coverage edge.
    pub size: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for StrokeFx {
    fn default() -> Self {
        StrokeFx {
            size: 3.0,
            color: [1.0, 1.0, 1.0],
            opacity: 1.0,
        }
    }
}

impl LayerEffects {
    pub fn is_empty(&self) -> bool {
        self.drop_shadow.is_none()
            && self.outer_glow.is_none()
            && self.color_overlay.is_none()
            && self.gradient_overlay.is_none()
            && self.inner_shadow.is_none()
            && self.inner_glow.is_none()
            && self.stroke.is_none()
    }

    /// How far (px) any effect reaches outside the layer's coverage — also
    /// how far one must read outside a tile to render it correctly, which
    /// is why the inner effects count too.
    pub fn pad(&self) -> i32 {
        let mut p = 0.0f32;
        for s in [&self.drop_shadow, &self.inner_shadow].into_iter().flatten() {
            p = p.max(sane_radius(s.blur) * 2.0 + s.dx.abs().max(s.dy.abs()));
        }
        for g in [&self.outer_glow, &self.inner_glow].into_iter().flatten() {
            p = p.max(sane_radius(g.blur) * 2.0);
        }
        if let Some(st) = &self.stroke {
            p = p.max(sane_radius(st.size) + 2.0);
        }
        p.ceil() as i32 + 2
    }
}

/// A per-layer mask. Coverage lives in the alpha channel of a sparse
/// [`TileStore`]; pixels with no tile take `default` (1.0 reveals, 0.0 hides).
#[derive(Clone, Debug)]
pub struct Mask {
    pub tiles: TileStore,
    pub default: f32,
    pub enabled: bool,
}

impl Mask {
    pub fn reveal_all() -> Self {
        Mask {
            tiles: TileStore::new(),
            default: 1.0,
            enabled: true,
        }
    }

    pub fn hide_all() -> Self {
        Mask {
            tiles: TileStore::new(),
            default: 0.0,
            enabled: true,
        }
    }

    /// Coverage at a pixel, 0.0 to 1.0.
    #[inline]
    pub fn value(&self, x: i32, y: i32) -> f32 {
        let c = lumenply_tiles::TileCoord::containing(x, y);
        match self.tiles.tile(c) {
            Some(t) => {
                let (ox, oy) = c.origin();
                t.get((x - ox) as usize, (y - oy) as usize).a
            }
            None => self.default,
        }
    }

    pub fn set_value(&mut self, x: i32, y: i32, v: f32) {
        let v = v.clamp(0.0, 1.0);
        let c = lumenply_tiles::TileCoord::containing(x, y);
        if self.tiles.tile(c).is_none() {
            // A fresh tile must start at the mask's default, not transparent,
            // or painting one pixel would hide the rest of its tile.
            let d = self.default;
            self.tiles.insert(
                c,
                std::sync::Arc::new(lumenply_tiles::Tile::filled(Rgba::new(d, d, d, d))),
            );
        }
        self.tiles.set_pixel(x, y, Rgba::new(v, v, v, v));
    }
}

/// Editable text. The glyphs are rasterised into `cache` by `lumenply-render`
/// whenever the text changes; the cache is never saved to disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextLayer {
    pub text: String,
    /// Baseline origin of the first line, in canvas pixels.
    pub x: f32,
    pub y: f32,
    /// Font size in pixels.
    pub size: f32,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    pub bold: bool,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Font family name or a path to a .ttf/.otf; empty means the bundled default.
    #[serde(default)]
    pub font: String,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub align: TextAlign,
    /// Extra space between glyphs, in thousandths of an em (Photoshop units).
    #[serde(default)]
    pub tracking: f32,
    #[serde(skip)]
    pub cache: Option<TileStore>,
}

/// Horizontal alignment of a text layer's lines relative to its anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl PartialEq for TextLayer {
    /// Compares the editable fields; the raster cache is derived state.
    fn eq(&self, o: &Self) -> bool {
        self.text == o.text
            && self.x == o.x
            && self.y == o.y
            && self.size == o.size
            && self.color == o.color
            && self.bold == o.bold
            && self.line_height == o.line_height
            && self.font == o.font
            && self.italic == o.italic
            && self.align == o.align
            && self.tracking == o.tracking
    }
}

impl TextLayer {
    pub fn new(text: impl Into<String>, x: f32, y: f32, size: f32, color: [f32; 4]) -> Self {
        TextLayer {
            text: text.into(),
            x,
            y,
            size,
            color,
            bold: false,
            line_height: 1.2,
            font: String::new(),
            italic: false,
            align: TextAlign::Left,
            tracking: 0.0,
            cache: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum LayerContent {
    Pixel(TileStore),
    Group(Vec<Layer>),
    Adjustment(Adjustment),
    /// A live filter applied to everything below it.
    Filter(Filter),
    /// Editable text, rasterised on demand.
    Text(TextLayer),
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    /// 0.0 (invisible) to 1.0 (opaque).
    pub opacity: f32,
    pub blend: BlendMode,
    /// Non-destructive effects rendered from this layer's coverage.
    pub effects: LayerEffects,
    /// Clip to the layer below: this layer shows only where the base of
    /// its clip chain has coverage, and the chain composites as one unit
    /// with the base's blend and opacity. Ignored on the bottom sibling.
    pub clip: bool,
    /// Groups only: composite the children straight onto the backdrop
    /// instead of as an isolated unit, so adjustments and blend modes
    /// inside the group reach the layers below it.
    pub pass_through: bool,
    pub mask: Option<Mask>,
    pub content: LayerContent,
    /// UI state for groups: children hidden in the layer list.
    pub collapsed: bool,
}

impl Layer {
    pub fn pixel(id: LayerId, name: impl Into<String>) -> Self {
        Layer::with_content(id, name, LayerContent::Pixel(TileStore::new()))
    }

    pub fn group(id: LayerId, name: impl Into<String>) -> Self {
        Layer::with_content(id, name, LayerContent::Group(Vec::new()))
    }

    pub fn adjustment(id: LayerId, adj: Adjustment) -> Self {
        let name = adj.name().to_string();
        Layer::with_content(id, name, LayerContent::Adjustment(adj))
    }

    pub fn filter(id: LayerId, f: Filter) -> Self {
        let name = f.name().to_string();
        Layer::with_content(id, name, LayerContent::Filter(f))
    }

    pub fn text(id: LayerId, t: TextLayer) -> Self {
        let name: String = t.text.lines().next().unwrap_or("Text").chars().take(24).collect();
        Layer::with_content(
            id,
            if name.is_empty() { "Text".into() } else { name },
            LayerContent::Text(t),
        )
    }

    pub fn text_layer(&self) -> Option<&TextLayer> {
        match &self.content {
            LayerContent::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn text_layer_mut(&mut self) -> Option<&mut TextLayer> {
        match &mut self.content {
            LayerContent::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Pixels to composite for this layer: own pixels, or a text layer's cache.
    pub fn raster_store(&self) -> Option<&TileStore> {
        match &self.content {
            LayerContent::Pixel(s) => Some(s),
            LayerContent::Text(t) => t.cache.as_ref(),
            _ => None,
        }
    }

    pub fn with_content(id: LayerId, name: impl Into<String>, content: LayerContent) -> Self {
        Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            effects: LayerEffects::default(),
            clip: false,
            pass_through: false,
            mask: None,
            content,
            collapsed: false,
        }
    }

    pub fn pixels(&self) -> Option<&TileStore> {
        match &self.content {
            LayerContent::Pixel(s) => Some(s),
            _ => None,
        }
    }

    pub fn pixels_mut(&mut self) -> Option<&mut TileStore> {
        match &mut self.content {
            LayerContent::Pixel(s) => Some(s),
            _ => None,
        }
    }

    pub fn children(&self) -> Option<&[Layer]> {
        match &self.content {
            LayerContent::Group(c) => Some(c),
            _ => None,
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<Layer>> {
        match &mut self.content {
            LayerContent::Group(c) => Some(c),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    /// The active selection, if any. Part of the document so it is covered
    /// by undo; not written to project files.
    pub selection: Option<Selection>,
    /// The pen tool's work path; covered by undo and saved with projects.
    pub work_path: Option<VectorPath>,
    /// Bottom-to-top.
    layers: Vec<Layer>,
    next_id: LayerId,
}

/// One anchor of a vector path: the point plus absolute cubic-bezier
/// handles. A corner node keeps both handles on the point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathNode {
    pub point: (f32, f32),
    pub handle_in: (f32, f32),
    pub handle_out: (f32, f32),
}

impl PathNode {
    pub fn corner(x: f32, y: f32) -> Self {
        PathNode {
            point: (x, y),
            handle_in: (x, y),
            handle_out: (x, y),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubPath {
    pub nodes: Vec<PathNode>,
    pub closed: bool,
}

/// A vector path: cubic bezier subpaths, drawn by the pen tool.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorPath {
    pub subpaths: Vec<SubPath>,
}

impl VectorPath {
    pub fn is_empty(&self) -> bool {
        self.subpaths.iter().all(|s| s.nodes.len() < 2)
    }

    /// Flatten every subpath into a dense polyline (point spacing roughly
    /// one pixel), returned with its `closed` flag. Fill, stroke, selection
    /// and the on-canvas preview all build on this.
    pub fn flatten(&self) -> Vec<(Vec<(f32, f32)>, bool)> {
        let mut out = Vec::new();
        for sp in &self.subpaths {
            if sp.nodes.len() < 2 {
                continue;
            }
            let mut pts: Vec<(f32, f32)> = vec![sp.nodes[0].point];
            let seg_count = sp.nodes.len() - usize::from(!sp.closed);
            for i in 0..seg_count {
                let a = &sp.nodes[i];
                let b = &sp.nodes[(i + 1) % sp.nodes.len()];
                flatten_cubic(a.point, a.handle_out, b.handle_in, b.point, &mut pts);
            }
            out.push((pts, sp.closed));
        }
        out
    }
}

/// Append a cubic segment to `out` (the start point is already there),
/// sampled finely enough that chords stay around a pixel long.
fn flatten_cubic(p0: (f32, f32), c0: (f32, f32), c1: (f32, f32), p1: (f32, f32), out: &mut Vec<(f32, f32)>) {
    let approx_len = dist(p0, c0) + dist(c0, c1) + dist(c1, p1);
    let steps = (approx_len.ceil() as usize).clamp(1, 1024);
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * c0.0 + 3.0 * u * t * t * c1.0 + t * t * t * p1.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * c0.1 + 3.0 * u * t * t * c1.1 + t * t * t * p1.1;
        out.push((x, y));
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

impl Document {
    pub fn new(width: u32, height: u32) -> Self {
        Document {
            width,
            height,
            selection: None,
            work_path: None,
            layers: Vec::new(),
            next_id: 1,
        }
    }

    /// Rebuild a document from its parts (used when loading a file).
    pub fn from_parts(width: u32, height: u32, layers: Vec<Layer>, next_id: LayerId) -> Self {
        Document {
            width,
            height,
            selection: None,
            work_path: None,
            layers,
            next_id,
        }
    }

    pub fn canvas(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// The id the next allocated layer will get.
    pub fn next_id(&self) -> LayerId {
        self.next_id
    }

    /// Top-level layers, bottom-to-top.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn layers_mut(&mut self) -> &mut Vec<Layer> {
        &mut self.layers
    }

    pub fn alloc_id(&mut self) -> LayerId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Push a layer on top of the stack. The layer's id must come from
    /// [`Document::alloc_id`] so it is unique within this document.
    pub fn add_layer(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        self.layers.push(layer);
        id
    }

    pub fn add_pixel_layer(&mut self, name: impl Into<String>) -> LayerId {
        let id = self.alloc_id();
        self.add_layer(Layer::pixel(id, name))
    }

    pub fn add_group(&mut self, name: impl Into<String>) -> LayerId {
        let id = self.alloc_id();
        self.add_layer(Layer::group(id, name))
    }

    pub fn add_adjustment(&mut self, adj: Adjustment) -> LayerId {
        let id = self.alloc_id();
        self.add_layer(Layer::adjustment(id, adj))
    }

    pub fn add_filter(&mut self, f: Filter) -> LayerId {
        let id = self.alloc_id();
        self.add_layer(Layer::filter(id, f))
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        find(&self.layers, id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        find_mut(&mut self.layers, id)
    }

    /// Remove a layer (anywhere in the tree) and return it.
    pub fn remove_layer(&mut self, id: LayerId) -> Option<Layer> {
        remove(&mut self.layers, id)
    }

    /// Number of layers in the whole tree, groups included.
    pub fn layer_count(&self) -> usize {
        count(&self.layers)
    }

    /// Visit every layer mutably, depth-first, bottom-to-top.
    pub fn for_each_layer_mut(&mut self, mut f: impl FnMut(&mut Layer)) {
        fn walk(layers: &mut [Layer], f: &mut impl FnMut(&mut Layer)) {
            for l in layers {
                f(l);
                if let Some(c) = l.children_mut() {
                    walk(c, f);
                }
            }
        }
        walk(&mut self.layers, &mut f);
    }

    /// The list of siblings that contains `id` (top-level or a group's children).
    pub fn siblings_mut(&mut self, id: LayerId) -> Option<&mut Vec<Layer>> {
        fn find(list: &mut Vec<Layer>, id: LayerId) -> Option<&mut Vec<Layer>> {
            if list.iter().any(|l| l.id == id) {
                return Some(list);
            }
            for l in list.iter_mut() {
                if let Some(c) = l.children_mut() {
                    if let Some(found) = find(c, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        find(&mut self.layers, id)
    }

    /// Id of the group containing `id`, or `None` at the top level.
    pub fn parent_of(&self, id: LayerId) -> Option<LayerId> {
        fn find(list: &[Layer], id: LayerId, parent: Option<LayerId>) -> Option<Option<LayerId>> {
            if list.iter().any(|l| l.id == id) {
                return Some(parent);
            }
            for l in list {
                if let Some(c) = l.children() {
                    if let Some(found) = find(c, id, Some(l.id)) {
                        return Some(found);
                    }
                }
            }
            None
        }
        find(&self.layers, id, None).flatten()
    }

    /// Visit every layer depth-first, bottom-to-top.
    pub fn for_each_layer(&self, mut f: impl FnMut(&Layer)) {
        fn walk(layers: &[Layer], f: &mut impl FnMut(&Layer)) {
            for l in layers {
                f(l);
                if let Some(c) = l.children() {
                    walk(c, f);
                }
            }
        }
        walk(&self.layers, &mut f);
    }
}

fn find(layers: &[Layer], id: LayerId) -> Option<&Layer> {
    for l in layers {
        if l.id == id {
            return Some(l);
        }
        if let Some(c) = l.children() {
            if let Some(found) = find(c, id) {
                return Some(found);
            }
        }
    }
    None
}

fn find_mut(layers: &mut [Layer], id: LayerId) -> Option<&mut Layer> {
    for l in layers {
        if l.id == id {
            return Some(l);
        }
        if let Some(c) = l.children_mut() {
            if let Some(found) = find_mut(c, id) {
                return Some(found);
            }
        }
    }
    None
}

fn remove(layers: &mut Vec<Layer>, id: LayerId) -> Option<Layer> {
    if let Some(i) = layers.iter().position(|l| l.id == id) {
        return Some(layers.remove(i));
    }
    for l in layers {
        if let Some(c) = l.children_mut() {
            if let Some(found) = remove(c, id) {
                return Some(found);
            }
        }
    }
    None
}

fn count(layers: &[Layer]) -> usize {
    layers.iter().map(|l| 1 + l.children().map_or(0, count)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_mode_names_round_trip() {
        for m in BlendMode::ALL {
            assert_eq!(m.name().parse::<BlendMode>().unwrap(), m);
        }
        assert_eq!("Hard_Light".parse::<BlendMode>().unwrap(), BlendMode::HardLight);
        assert!("glow".parse::<BlendMode>().is_err());
    }

    #[test]
    fn mask_defaults_outside_painted_tiles() {
        let mut m = Mask::reveal_all();
        assert_eq!(m.value(5000, 5000), 1.0);
        m.set_value(3, 3, 0.25);
        assert_eq!(m.value(3, 3), 0.25);
        assert_eq!(m.value(4, 3), 1.0); // same tile, untouched pixels keep the default
        assert_eq!(Mask::hide_all().value(0, 0), 0.0);
    }

    #[test]
    fn layers_are_found_inside_groups() {
        let mut d = Document::new(10, 10);
        let bg = d.add_pixel_layer("Background");
        let g = d.add_group("Group");
        let inner_id = d.alloc_id();
        d.layer_mut(g)
            .unwrap()
            .children_mut()
            .unwrap()
            .push(Layer::pixel(inner_id, "Inner"));

        assert_eq!(d.layer_count(), 3);
        assert_eq!(d.layer(inner_id).unwrap().name, "Inner");
        d.layer_mut(inner_id).unwrap().opacity = 0.5;
        assert_eq!(d.layer(inner_id).unwrap().opacity, 0.5);

        assert!(d.remove_layer(inner_id).is_some());
        assert_eq!(d.layer_count(), 2);
        assert!(d.layer(bg).is_some());
        assert!(d.layer(999).is_none());
    }
}
