//! Built-in commands. Each is a plain data struct so it can be constructed
//! from the UI, from a script, or deserialised from a macro file.

use lumenply_doc::{
    Adjustment, BlendMode, CombineOp, Document, Filter, Layer, LayerContent, LayerId, Mask, Selection,
    TextLayer,
};
use lumenply_tiles::{Affine, Raster, Rect, Rgba, TileStore};

use crate::{Command, EditError, EditResult, Motion};

/// Add an empty pixel layer (or one filled from a raster) on top of the stack.
pub struct AddPixelLayer {
    pub name: String,
    /// Optional initial pixels and their top-left position on the canvas.
    pub pixels: Option<(Raster, i32, i32)>,
    pub blend: BlendMode,
    pub opacity: f32,
}

impl AddPixelLayer {
    pub fn new(name: impl Into<String>) -> Self {
        AddPixelLayer {
            name: name.into(),
            pixels: None,
            blend: BlendMode::Normal,
            opacity: 1.0,
        }
    }

    pub fn from_raster(name: impl Into<String>, raster: Raster, x: i32, y: i32) -> Self {
        let mut c = AddPixelLayer::new(name);
        c.pixels = Some((raster, x, y));
        c
    }
}

impl Command for AddPixelLayer {
    fn label(&self) -> String {
        format!("Add layer '{}'", self.name)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let id = doc.alloc_id();
        let mut layer = Layer::pixel(id, self.name.clone());
        layer.blend = self.blend;
        layer.opacity = self.opacity.clamp(0.0, 1.0);
        if let Some((raster, x, y)) = &self.pixels {
            *layer.pixels_mut().expect("pixel layer") = TileStore::from_raster(raster, *x, *y);
        }
        doc.add_layer(layer);
        Ok(())
    }
}

/// Add an adjustment layer on top of the stack.
pub struct AddAdjustmentLayer {
    pub adjustment: Adjustment,
    pub opacity: f32,
    pub blend: BlendMode,
    /// Insert directly above this layer instead of on top of the stack.
    pub above: Option<LayerId>,
}

impl AddAdjustmentLayer {
    pub fn new(adjustment: Adjustment) -> Self {
        AddAdjustmentLayer {
            adjustment,
            opacity: 1.0,
            blend: BlendMode::Normal,
            above: None,
        }
    }
}

impl Command for AddAdjustmentLayer {
    fn label(&self) -> String {
        format!("Add {} layer", self.adjustment.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let id = doc.alloc_id();
        let mut layer = Layer::adjustment(id, self.adjustment.clone());
        layer.opacity = self.opacity.clamp(0.0, 1.0);
        layer.blend = self.blend;
        insert_above(doc, layer, self.above)
    }
}

/// Insert `layer` directly above `above` (same sibling list), or on top.
fn insert_above(doc: &mut Document, layer: Layer, above: Option<LayerId>) -> EditResult {
    match above.and_then(|a| doc.siblings_mut(a).map(|list| (a, list))) {
        Some((a, list)) => {
            let i = list.iter().position(|l| l.id == a).expect("in siblings");
            list.insert(i + 1, layer);
            Ok(())
        }
        None => {
            doc.add_layer(layer);
            Ok(())
        }
    }
}

/// Add a live filter layer, above `above` or on top of the stack.
pub struct AddFilterLayer {
    pub filter: Filter,
    pub opacity: f32,
    pub above: Option<LayerId>,
}

impl AddFilterLayer {
    pub fn new(filter: Filter) -> Self {
        AddFilterLayer {
            filter,
            opacity: 1.0,
            above: None,
        }
    }
}

impl Command for AddFilterLayer {
    fn label(&self) -> String {
        format!("Add {} layer", self.filter.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let id = doc.alloc_id();
        let mut layer = Layer::filter(id, self.filter.clone());
        layer.opacity = self.opacity.clamp(0.0, 1.0);
        insert_above(doc, layer, self.above)
    }
}

/// Add an editable text layer, above `above` or on top of the stack.
pub struct AddTextLayer {
    pub text: TextLayer,
    pub above: Option<LayerId>,
}

impl Command for AddTextLayer {
    fn label(&self) -> String {
        "Add text".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let id = doc.alloc_id();
        let mut t = self.text.clone();
        lumenply_render::text::refresh_cache(&mut t);
        insert_above(doc, Layer::text(id, t), self.above)
    }
}

/// Replace a text layer's content or style (re-rasterises).
pub struct SetText {
    pub layer: LayerId,
    pub text: TextLayer,
}

impl Command for SetText {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Edit text".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        // Old glyphs plus new glyphs.
        let old = doc.layer(self.layer)?.raster_store()?.bounds();
        let mut t = self.text.clone();
        lumenply_render::text::refresh_cache(&mut t);
        let new = t.cache.as_ref()?.bounds();
        match (old, new) {
            (Some(a), Some(b)) => Some(a.union(&b).intersect(&doc.canvas())),
            (Some(a), None) | (None, Some(a)) => Some(a.intersect(&doc.canvas())),
            (None, None) => Some(Rect::default()),
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let t = l
            .text_layer_mut()
            .ok_or_else(|| EditError::Invalid(format!("layer {} is not a text layer", self.layer)))?;
        *t = self.text.clone();
        lumenply_render::text::refresh_cache(t);
        let first_line: String = t.text.lines().next().unwrap_or("Text").chars().take(24).collect();
        if !first_line.is_empty() {
            l.name = first_line;
        }
        Ok(())
    }
}

/// Convert a text layer into an ordinary pixel layer.
/// Compose `t` onto a smart layer and re-render its cache from the source.
fn smart_compose(s: &mut lumenply_doc::SmartLayer, t: &Affine) {
    s.transform = s.transform.then(t);
    s.cache = Some(lumenply_render::transform_store(&s.source, &s.transform));
}

/// Wrap a pixel layer's pixels into a smart object: from here on,
/// transforms compose and re-render from this untouched source, so
/// repeated scaling or rotating never degrades.
pub struct ConvertToSmartObject {
    pub layer: LayerId,
}

impl Command for ConvertToSmartObject {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Convert to smart object".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        // The pixels do not change, only what future edits do to them.
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = match &mut l.content {
            LayerContent::Pixel(s) => std::mem::take(s),
            LayerContent::Smart(_) => {
                return Err(EditError::Invalid("the layer is already a smart object".into()))
            }
            _ => return Err(EditError::NotPixel(self.layer)),
        };
        l.content = LayerContent::Smart(lumenply_doc::SmartLayer {
            cache: Some(store.clone()),
            source: store,
            transform: Affine::IDENTITY,
        });
        Ok(())
    }
}

pub struct RasterizeLayer {
    pub layer: LayerId,
}

impl Command for RasterizeLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Rasterize".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = match &mut l.content {
            LayerContent::Text(t) => {
                lumenply_render::text::refresh_cache(t);
                t.cache.take().unwrap_or_default()
            }
            LayerContent::Smart(sm) => sm
                .cache
                .take()
                .unwrap_or_else(|| lumenply_render::transform_store(&sm.source, &sm.transform)),
            _ => {
                return Err(EditError::Invalid(format!(
                    "layer {} cannot be rasterized",
                    self.layer
                )))
            }
        };
        l.content = LayerContent::Pixel(store);
        Ok(())
    }
}

/// Replace a filter layer's parameters.
pub struct SetFilter {
    pub layer: LayerId,
    pub filter: Filter,
}

impl Command for SetFilter {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        format!("Edit {}", self.filter.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        match &mut l.content {
            LayerContent::Filter(f) => {
                *f = self.filter.clone();
                Ok(())
            }
            _ => Err(EditError::Invalid(format!(
                "layer {} is not a filter layer",
                self.layer
            ))),
        }
    }
}

/// Replace a layer's adjustment parameters (the "edit adjustment" dialog).
pub struct SetAdjustment {
    pub layer: LayerId,
    pub adjustment: Adjustment,
}

impl Command for SetAdjustment {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        format!("Edit {}", self.adjustment.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        match &mut l.content {
            lumenply_doc::LayerContent::Adjustment(a) => {
                *a = self.adjustment.clone();
                Ok(())
            }
            _ => Err(EditError::Invalid(format!(
                "layer {} is not an adjustment layer",
                self.layer
            ))),
        }
    }
}

/// Attach, replace or remove a layer's mask.
pub struct SetMask {
    pub layer: LayerId,
    pub mask: Option<Mask>,
}

impl Command for SetMask {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.mask.is_some() {
            "Set mask"
        } else {
            "Remove mask"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.mask = self.mask.clone();
        Ok(())
    }
}

/// Replace the active selection (`None` deselects).
pub struct SetSelection {
    pub selection: Option<Selection>,
}

impl Command for SetSelection {
    fn label(&self) -> String {
        if self.selection.is_some() {
            "Select"
        } else {
            "Deselect"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        doc.selection = self.selection.clone().filter(|s| !s.is_empty());
        Ok(())
    }
}

/// Combine a shape with the active selection (or start one).
pub struct ModifySelection {
    pub shape: Selection,
    pub op: CombineOp,
}

impl Command for ModifySelection {
    fn label(&self) -> String {
        match self.op {
            CombineOp::Replace => "Select",
            CombineOp::Union => "Add to selection",
            CombineOp::Intersect => "Intersect selection",
            CombineOp::Subtract => "Subtract from selection",
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let mut sel = doc.selection.take().unwrap_or_else(Selection::none);
        sel.combine(&self.shape, self.op);
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

pub struct InvertSelection;

impl Command for InvertSelection {
    fn label(&self) -> String {
        "Invert selection".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        match doc.selection.as_mut() {
            Some(s) => {
                s.invert();
                Ok(())
            }
            None => {
                doc.selection = Some(Selection::all());
                Ok(())
            }
        }
    }
}

pub struct FeatherSelection {
    pub radius: f32,
}

impl Command for FeatherSelection {
    fn label(&self) -> String {
        format!("Feather {:.0} px", self.radius)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let s = doc
            .selection
            .as_mut()
            .ok_or_else(|| EditError::Invalid("nothing is selected".into()))?;
        s.feather(self.radius);
        Ok(())
    }
}

/// Fill the selection (or the whole canvas) on a pixel layer with a colour.
pub struct Fill {
    pub layer: LayerId,
    /// Straight linear RGBA.
    pub color: [f32; 4],
}

impl Command for Fill {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Fill".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(
            doc.selection
                .as_ref()
                .map_or(doc.canvas(), |s| s.bounds_within(doc.canvas())),
        )
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let area = sel.as_ref().map_or(canvas, |s| s.bounds_within(canvas));
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let [r, g, b, a] = self.color;
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let cov = sel.as_ref().map_or(1.0, |s| s.value(px, py));
                if cov <= 0.0 {
                    continue;
                }
                let src = Rgba::from_straight(r, g, b, a * cov);
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, src.over(dst));
            }
        }
        store.prune_blank();
        Ok(())
    }
}

/// Erase the selection (or the whole layer) on a pixel layer.
pub struct Clear {
    pub layer: LayerId,
}

impl Command for Clear {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Clear".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(
            doc.selection
                .as_ref()
                .map_or(doc.canvas(), |s| s.bounds_within(doc.canvas())),
        )
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let Some(sel) = sel else {
            *store = TileStore::new();
            return Ok(());
        };
        let area = sel.bounds_within(canvas);
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let cov = sel.value(px, py);
                if cov <= 0.0 {
                    continue;
                }
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, dst.scale(1.0 - cov));
            }
        }
        store.prune_blank();
        Ok(())
    }
}

/// Turn the active selection into the layer's mask.
pub struct MaskFromSelection {
    pub layer: LayerId,
}

impl Command for MaskFromSelection {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Mask from selection".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .selection
            .as_ref()
            .ok_or_else(|| EditError::Invalid("nothing is selected".into()))?;
        let mask = sel.to_mask();
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.mask = Some(mask);
        Ok(())
    }
}

/// Rename a layer.
/// Copy a layer's selected pixels onto a new layer directly above it
/// ("layer via copy"). Without a selection the whole layer is copied.
pub struct NewLayerFromSelection {
    pub layer: LayerId,
    pub name: String,
}

impl Command for NewLayerFromSelection {
    fn label(&self) -> String {
        "Layer via copy".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let src = doc.layer(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = src.pixels().ok_or(EditError::NotPixel(self.layer))?;
        let mut copy = match &doc.selection {
            None => store.clone(),
            Some(sel) => {
                // Keep only the covered pixels, weighted by coverage.
                let mut out = lumenply_tiles::TileStore::new();
                let bounds = sel.bounds_within(doc.canvas());
                for c in store.coords() {
                    if c.rect().intersect(&bounds).is_empty() {
                        continue;
                    }
                    let Some(tile) = store.tile(c) else { continue };
                    let mut t = tile.clone();
                    let (ox, oy) = c.origin();
                    let px = t.pixels_mut();
                    for (i, p) in px.iter_mut().enumerate() {
                        let (x, y) = (
                            ox + (i % lumenply_tiles::TILE_SIZE) as i32,
                            oy + (i / lumenply_tiles::TILE_SIZE) as i32,
                        );
                        let v = sel.value(x, y);
                        if v < 1.0 {
                            *p = p.scale(v);
                        }
                    }
                    out.insert(c, std::sync::Arc::new(t));
                }
                out.prune_blank();
                out
            }
        };
        copy.compact();
        let id = doc.alloc_id();
        let mut l = Layer::pixel(id, self.name.clone());
        *l.pixels_mut().expect("pixel layer") = copy;
        insert_above(doc, l, Some(self.layer))
    }
}

pub struct RenameLayer {
    pub layer: LayerId,
    pub name: String,
}

impl Command for RenameLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Rename layer".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.name = self.name.trim().to_string();
        if l.name.is_empty() {
            l.name = "Layer".into();
        }
        Ok(())
    }
}

/// Move a layer up (`+1`) or down (`-1`) among its siblings.
pub struct ReorderLayer {
    pub layer: LayerId,
    pub delta: i32,
}

impl Command for ReorderLayer {
    fn label(&self) -> String {
        if self.delta > 0 {
            "Move layer up"
        } else {
            "Move layer down"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        fn reorder(list: &mut Vec<Layer>, id: LayerId, delta: i32) -> Option<bool> {
            if let Some(i) = list.iter().position(|l| l.id == id) {
                let j = (i as i32 + delta).clamp(0, list.len() as i32 - 1) as usize;
                if j != i {
                    let l = list.remove(i);
                    list.insert(j, l);
                    return Some(true);
                }
                return Some(false);
            }
            for l in list.iter_mut() {
                if let Some(c) = l.children_mut() {
                    if let Some(r) = reorder(c, id, delta) {
                        return Some(r);
                    }
                }
            }
            None
        }
        match reorder(doc.layers_mut(), self.layer, self.delta) {
            Some(_) => Ok(()),
            None => Err(EditError::NoLayer(self.layer)),
        }
    }
}

/// Shift a pixel layer (and its mask) by whole pixels. Exact, no resampling.
/// Move a layer to an arbitrary position: into the sibling list of
/// `parent` (`None` = the root) at `index`, counted bottom-to-top and
/// clamped. Drives drag-and-drop in the layer panel.
pub struct RelocateLayer {
    pub layer: LayerId,
    pub parent: Option<LayerId>,
    pub index: usize,
}

impl Command for RelocateLayer {
    fn label(&self) -> String {
        "Move layer".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        // A group may not be dropped into itself or its own subtree.
        if let Some(p) = self.parent {
            let inside = doc
                .layer(self.layer)
                .is_some_and(|l| l.id == p || l.children().is_some_and(|_| contains_id(l, p)));
            if inside {
                return Err(EditError::Invalid("cannot move a group into itself".into()));
            }
        }
        let moved = doc
            .remove_layer(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?;
        let list = match self.parent {
            None => Some(doc.layers_mut()),
            Some(p) => match doc.layer_mut(p) {
                // The parent may have vanished only if it sat inside the
                // moved subtree, which the check above rejects.
                Some(l) => l.children_mut(),
                None => None,
            },
        };
        let Some(list) = list else {
            // Put it back where the stack is always valid: on top of root.
            doc.layers_mut().push(moved);
            return Err(EditError::NotGroup(self.parent.unwrap_or_default()));
        };
        let i = self.index.min(list.len());
        list.insert(i, moved);
        Ok(())
    }
}

fn contains_id(l: &Layer, id: LayerId) -> bool {
    l.id == id
        || l.children()
            .is_some_and(|c| c.iter().any(|ch| contains_id(ch, id)))
}

pub struct MoveLayer {
    pub layer: LayerId,
    pub dx: i32,
    pub dy: i32,
}

impl Command for MoveLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        Motion::Translate
    }

    fn label(&self) -> String {
        "Move".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        TransformLayer {
            layer: self.layer,
            transform: Affine::translate(self.dx as f32, self.dy as f32),
        }
        .apply(doc)
    }
}

/// Apply an affine transform to a pixel layer's pixels and mask.
pub struct TransformLayer {
    pub layer: LayerId,
    pub transform: Affine,
}

impl TransformLayer {
    /// Scale and rotate around the centre of the layer's painted bounds.
    pub fn around_center(doc: &Document, layer: LayerId, sx: f32, sy: f32, radians: f32) -> Option<Self> {
        let l = doc.layer(layer)?;
        let b = l.pixels()?.content_bounds()?;
        let cx = b.x as f32 + b.w as f32 / 2.0;
        let cy = b.y as f32 + b.h as f32 / 2.0;
        Some(TransformLayer {
            layer,
            transform: Affine::around(cx, cy, sx, sy, radians),
        })
    }
}

impl Command for TransformLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        match self.transform.integer_translation() {
            Some(_) => Motion::Translate,
            None => Motion::Reshape,
        }
    }

    fn label(&self) -> String {
        if self.transform.integer_translation().is_some() {
            "Move".into()
        } else {
            "Transform".into()
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.transform.inverse().is_none() {
            return Err(EditError::Invalid("transform is not invertible".into()));
        }
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        match &mut l.content {
            LayerContent::Pixel(store) => {
                *store = lumenply_render::transform_store(store, &self.transform);
            }
            LayerContent::Smart(sm) => smart_compose(sm, &self.transform),
            LayerContent::Text(t) => match self.transform.integer_translation() {
                Some((dx, dy)) => {
                    t.x += dx as f32;
                    t.y += dy as f32;
                    lumenply_render::text::refresh_cache(t);
                }
                None => {
                    return Err(EditError::Invalid(
                        "rasterize the text layer before scaling or rotating it".into(),
                    ))
                }
            },
            _ => return Err(EditError::NotPixel(self.layer)),
        }
        if let Some(m) = l.mask.as_mut() {
            *m = lumenply_render::transform_mask(m, &self.transform);
        }
        Ok(())
    }
}

/// Warp a pixel layer so its painted bounds land on `quad` (top-left,
/// top-right, bottom-right, bottom-left). The mask warps through the same
/// homography, so it stays registered with the pixels.
pub struct PerspectiveLayer {
    pub layer: LayerId,
    pub quad: [(f32, f32); 4],
}

impl Command for PerspectiveLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        Motion::Reshape
    }

    fn label(&self) -> String {
        "Perspective".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let old = doc
            .layer(self.layer)
            .and_then(|l| l.pixels())
            .and_then(|s| s.content_bounds())?;
        let xs = self.quad.iter().map(|p| p.0);
        let ys = self.quad.iter().map(|p| p.1);
        let x0 = xs.clone().fold(f32::INFINITY, f32::min).floor() as i32 - 1;
        let x1 = xs.fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1;
        let y0 = ys.clone().fold(f32::INFINITY, f32::min).floor() as i32 - 1;
        let y1 = ys.fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1;
        if x1 <= x0 || y1 <= y0 {
            return Some(old);
        }
        let new = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
        Some(old.union(&new).intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.quad.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
            return Err(EditError::Invalid("perspective corners must be finite".into()));
        }
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = match &mut l.content {
            LayerContent::Pixel(store) => store,
            LayerContent::Text(_) => {
                return Err(EditError::Invalid(
                    "rasterize the text layer before a perspective warp".into(),
                ))
            }
            LayerContent::Smart(_) => {
                return Err(EditError::Invalid(
                    "smart objects keep affine transforms only; rasterize before a perspective warp".into(),
                ))
            }
            _ => return Err(EditError::NotPixel(self.layer)),
        };
        let bounds = store
            .content_bounds()
            .ok_or_else(|| EditError::Invalid("the layer has no pixels to warp".into()))?;
        let h = lumenply_render::Homography::rect_to_quad(bounds, self.quad)
            .ok_or_else(|| EditError::Invalid("the corners form a degenerate quad".into()))?;
        if h.inverse().is_none() {
            return Err(EditError::Invalid("the corners form a degenerate quad".into()));
        }
        *store = lumenply_render::perspective_store_h(store, &h);
        if let Some(m) = l.mask.as_mut() {
            *m = lumenply_render::perspective_mask(m, &h);
        }
        Ok(())
    }
}

/// Bend a pixel layer through a warp mesh laid over its painted bounds.
/// The mask warps through the same mesh, so it stays registered.
pub struct WarpLayer {
    pub layer: LayerId,
    pub grid: lumenply_render::WarpGrid,
}

impl Command for WarpLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        Motion::Reshape
    }

    fn label(&self) -> String {
        "Warp".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let old = doc
            .layer(self.layer)
            .and_then(|l| l.pixels())
            .and_then(|s| s.content_bounds())?;
        if !self.grid.is_valid() {
            return Some(old);
        }
        let xs = self.grid.points.iter().map(|p| p.0);
        let ys = self.grid.points.iter().map(|p| p.1);
        let x0 = xs.clone().fold(f32::INFINITY, f32::min).floor() as i32 - 1;
        let x1 = xs.fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1;
        let y0 = ys.clone().fold(f32::INFINITY, f32::min).floor() as i32 - 1;
        let y1 = ys.fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1;
        let new = Rect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        Some(old.union(&new).intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if !self.grid.is_valid() {
            return Err(EditError::Invalid("the warp mesh is malformed".into()));
        }
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = match &mut l.content {
            LayerContent::Pixel(store) => store,
            LayerContent::Text(_) => {
                return Err(EditError::Invalid(
                    "rasterize the text layer before warping it".into(),
                ))
            }
            LayerContent::Smart(_) => {
                return Err(EditError::Invalid(
                    "smart objects keep affine transforms only; rasterize before warping".into(),
                ))
            }
            _ => return Err(EditError::NotPixel(self.layer)),
        };
        if store.content_bounds().is_none() {
            return Err(EditError::Invalid("the layer has no pixels to warp".into()));
        }
        *store = lumenply_render::warp_store(store, &self.grid);
        if let Some(m) = l.mask.as_mut() {
            *m = lumenply_render::warp_mask(m, &self.grid);
        }
        Ok(())
    }
}

/// Mirror a pixel layer about the vertical or horizontal axis of its bounds.
pub struct FlipLayer {
    pub layer: LayerId,
    pub horizontal: bool,
}

impl Command for FlipLayer {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn motion(&self) -> Motion {
        Motion::Reshape
    }

    fn label(&self) -> String {
        if self.horizontal {
            "Flip horizontal"
        } else {
            "Flip vertical"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = l.pixels().ok_or(EditError::NotPixel(self.layer))?;
        let Some(b) = store.content_bounds() else {
            return Ok(());
        };
        let (sx, sy) = if self.horizontal { (-1.0, 1.0) } else { (1.0, -1.0) };
        let cx = b.x as f32 + b.w as f32 / 2.0;
        let cy = b.y as f32 + b.h as f32 / 2.0;
        TransformLayer {
            layer: self.layer,
            transform: Affine::around(cx, cy, sx, sy, 0.0),
        }
        .apply(doc)
    }
}

/// Give a layer a mask: from the selection if there is one, else "reveal all".
pub struct AddMask {
    pub layer: LayerId,
}

impl Command for AddMask {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Add mask".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let mask = doc
            .selection
            .as_ref()
            .map_or_else(Mask::reveal_all, |s| s.to_mask());
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.mask = Some(mask);
        Ok(())
    }
}

pub struct RemoveMask {
    pub layer: LayerId,
}

impl Command for RemoveMask {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Remove mask".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.mask = None;
        Ok(())
    }
}

pub struct SetMaskEnabled {
    pub layer: LayerId,
    pub enabled: bool,
}

impl Command for SetMaskEnabled {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.enabled {
            "Enable mask"
        } else {
            "Disable mask"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let m = l
            .mask
            .as_mut()
            .ok_or_else(|| EditError::Invalid(format!("layer {} has no mask", self.layer)))?;
        m.enabled = self.enabled;
        Ok(())
    }
}

/// Wrap sibling layers in a new group placed where the lowest of them was.
pub struct GroupLayers {
    pub layers: Vec<LayerId>,
    pub name: String,
}

impl Command for GroupLayers {
    fn label(&self) -> String {
        "Group layers".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let first = *self
            .layers
            .first()
            .ok_or_else(|| EditError::Invalid("no layers to group".into()))?;
        let id = doc.alloc_id();
        let list = doc.siblings_mut(first).ok_or(EditError::NoLayer(first))?;
        if !self.layers.iter().all(|l| list.iter().any(|x| x.id == *l)) {
            return Err(EditError::Invalid("layers to group must be siblings".into()));
        }
        let lowest = list
            .iter()
            .position(|l| self.layers.contains(&l.id))
            .expect("checked above");
        let mut children = Vec::new();
        let mut i = 0;
        while i < list.len() {
            if self.layers.contains(&list[i].id) {
                children.push(list.remove(i));
            } else {
                i += 1;
            }
        }
        let mut group = Layer::group(id, self.name.clone());
        *group.children_mut().expect("group") = children;
        list.insert(lowest, group);
        Ok(())
    }
}

/// Replace a group with its children, in place.
pub struct UngroupLayer {
    pub layer: LayerId,
}

impl Command for UngroupLayer {
    fn label(&self) -> String {
        "Ungroup".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let list = doc
            .siblings_mut(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?;
        let i = list.iter().position(|l| l.id == self.layer).expect("in list");
        match list[i].content {
            LayerContent::Group(_) => {}
            _ => return Err(EditError::NotGroup(self.layer)),
        }
        let group = list.remove(i);
        let children = match group.content {
            LayerContent::Group(c) => c,
            _ => unreachable!(),
        };
        for (k, child) in children.into_iter().enumerate() {
            list.insert(i + k, child);
        }
        Ok(())
    }
}

/// Replace a layer's non-destructive effects (sliders coalesce through
/// this).
pub struct SetLayerEffects {
    pub layer: LayerId,
    pub effects: lumenply_doc::LayerEffects,
}

impl Command for SetLayerEffects {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Layer effects".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.effects = self.effects.clone();
        Ok(())
    }
}

/// Clip a layer to the one below it (or release the clip).
pub struct SetClipped {
    pub layer: LayerId,
    pub clip: bool,
}

impl Command for SetClipped {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.clip {
            "Clip to layer below".into()
        } else {
            "Release clip".into()
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.clip = self.clip;
        Ok(())
    }
}

/// Toggle a group's pass-through compositing.
pub struct SetPassThrough {
    pub layer: LayerId,
    pub pass_through: bool,
}

impl Command for SetPassThrough {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.pass_through {
            "Pass through".into()
        } else {
            "Isolate group".into()
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        if l.children().is_none() {
            return Err(EditError::NotGroup(self.layer));
        }
        l.pass_through = self.pass_through;
        Ok(())
    }
}

pub struct SetCollapsed {
    pub layer: LayerId,
    pub collapsed: bool,
}

impl Command for SetCollapsed {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.collapsed {
            "Collapse group"
        } else {
            "Expand group"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.collapsed = self.collapsed;
        Ok(())
    }
}

/// Run a destructive pixel filter on a layer (inside the selection, if any).
pub struct ApplyFilter {
    pub layer: LayerId,
    pub filter: Filter,
}

impl Command for ApplyFilter {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        self.filter.name().into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc.selection.clone();
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = l.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let filtered = lumenply_render::apply_filter(store, &self.filter);
        match sel {
            None => *store = filtered,
            Some(sel) => {
                // Blend filtered over original by selection coverage.
                let Some(b) = store
                    .bounds()
                    .map(|b| b.union(&filtered.bounds().unwrap_or_default()))
                else {
                    return Ok(());
                };
                let mut out = store.clone();
                for y in b.y..b.bottom() {
                    for x in b.x..b.right() {
                        let k = sel.value(x, y);
                        if k <= 0.0 {
                            continue;
                        }
                        let o = store.get_pixel(x, y);
                        let f = filtered.get_pixel(x, y);
                        out.set_pixel(
                            x,
                            y,
                            Rgba::new(
                                o.r + (f.r - o.r) * k,
                                o.g + (f.g - o.g) * k,
                                o.b + (f.b - o.b) * k,
                                o.a + (f.a - o.a) * k,
                            ),
                        );
                    }
                }
                out.prune_blank();
                *store = out;
            }
        }
        Ok(())
    }
}

/// Crop the canvas to `rect`. Pixels outside are kept (just off-canvas), so
/// the crop is reversible by enlarging the canvas again.
pub struct CropDocument {
    pub rect: Rect,
}

impl Command for CropDocument {
    fn label(&self) -> String {
        "Crop".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.rect.is_empty() {
            return Err(EditError::Invalid("crop area is empty".into()));
        }
        let (dx, dy) = (-self.rect.x, -self.rect.y);
        shift_all(doc, dx, dy);
        doc.width = self.rect.w;
        doc.height = self.rect.h;
        doc.selection = None;
        Ok(())
    }
}

/// Change the canvas size without scaling; `anchor` (0..1, 0..1) says where
/// the old content sits in the new canvas (0.5, 0.5 = centred).
pub struct ResizeCanvas {
    pub width: u32,
    pub height: u32,
    pub anchor: (f32, f32),
}

impl Command for ResizeCanvas {
    fn label(&self) -> String {
        "Canvas size".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.width == 0 || self.height == 0 {
            return Err(EditError::Invalid("canvas size must be positive".into()));
        }
        let dx = ((self.width as f32 - doc.width as f32) * self.anchor.0).round() as i32;
        let dy = ((self.height as f32 - doc.height as f32) * self.anchor.1).round() as i32;
        shift_all(doc, dx, dy);
        doc.width = self.width;
        doc.height = self.height;
        doc.selection = None;
        Ok(())
    }
}

/// Scale the whole image (every pixel layer and mask) to a new size.
pub struct ResizeImage {
    pub width: u32,
    pub height: u32,
}

impl Command for ResizeImage {
    fn label(&self) -> String {
        "Image size".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.width == 0 || self.height == 0 {
            return Err(EditError::Invalid("image size must be positive".into()));
        }
        let t = Affine::scale(
            self.width as f32 / doc.width as f32,
            self.height as f32 / doc.height as f32,
        );
        doc.for_each_layer_mut(|l| {
            match &mut l.content {
                LayerContent::Pixel(store) => *store = lumenply_render::transform_store(store, &t),
                LayerContent::Smart(sm) => smart_compose(sm, &t),
                _ => {}
            }
            if let Some(m) = l.mask.as_mut() {
                *m = lumenply_render::transform_mask(m, &t);
            }
        });
        doc.width = self.width;
        doc.height = self.height;
        doc.selection = None;
        Ok(())
    }
}

fn shift_all(doc: &mut Document, dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    doc.for_each_layer_mut(|l| {
        match &mut l.content {
            LayerContent::Pixel(store) => *store = store.translated(dx, dy),
            LayerContent::Smart(sm) => smart_compose(sm, &Affine::translate(dx as f32, dy as f32)),
            _ => {}
        }
        if let Some(m) = l.mask.as_mut() {
            *m = lumenply_render::transform_mask(m, &Affine::translate(dx as f32, dy as f32));
        }
    });
}

/// Which pixels a flood-based tool compares against: one layer or the
/// whole composite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleSource {
    Layer(LayerId),
    Merged,
}

/// Paint the selection itself (quick-mask mode): the brush colour's
/// luminance is the target coverage — white selects, black deselects,
/// the eraser always deselects. The current selection never clips these
/// strokes, since it is what they edit.
pub struct PaintSelection {
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
}

impl Command for PaintSelection {
    fn label(&self) -> String {
        "Paint selection".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let mut sel = doc.selection.take().unwrap_or_else(Selection::none);
        let [cr, cg, cb, ca] = self.brush.color;
        let target = match self.brush.mode {
            BrushMode::Erase | BrushMode::Burn | BrushMode::Desaturate => 0.0,
            _ => lumenply_doc::adjust::luminance(cr, cg, cb).clamp(0.0, 1.0),
        };
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, None, |px, py, cover| {
                let k = (ca * cover).clamp(0.0, 1.0);
                let v = sel.coverage.value(px, py);
                sel.coverage.set_value(px, py, v + (target - v) * k);
            });
        }
        sel.coverage.prune_uniform();
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

/// Select every pixel of the composite whose colour lies within
/// `tolerance` of `color` (straight linear RGB, max channel difference),
/// with coverage ramping down linearly across the top half of the
/// tolerance so edges come out soft.
pub struct SelectColorRange {
    pub color: [f32; 3],
    pub tolerance: f32,
}

impl Command for SelectColorRange {
    fn label(&self) -> String {
        "Colour range".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let tol = self.tolerance.clamp(0.0, 1.0);
        if tol <= 0.0 {
            return Err(EditError::Invalid("tolerance must be positive".into()));
        }
        let flat = lumenply_render::composite_raster(doc);
        let mut mask = Mask::hide_all();
        let ramp = (tol * 0.5).max(1e-4);
        for y in 0..flat.height as i32 {
            for x in 0..flat.width as i32 {
                let p = flat.get(x as u32, y as u32);
                if p.a <= 0.0 {
                    continue;
                }
                let [r, g, b, _] = p.to_straight();
                let d = (r - self.color[0])
                    .abs()
                    .max((g - self.color[1]).abs())
                    .max((b - self.color[2]).abs());
                let cover = ((tol - d) / ramp).clamp(0.0, 1.0);
                if cover > 0.0 {
                    mask.set_value(x, y, cover);
                }
            }
        }
        mask.prune_uniform();
        let sel = Selection { coverage: mask };
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

/// Grow a region from `seed` over `canvas`: every pixel whose colour is
/// within `tolerance` (max channel difference, straight RGBA, 0..1) of the
/// seed colour. `contiguous` restricts it to the connected area.
pub fn flood_region(
    sample: impl Fn(i32, i32) -> Rgba,
    canvas: Rect,
    seed: (i32, i32),
    tolerance: f32,
    contiguous: bool,
) -> Mask {
    let mut mask = Mask::hide_all();
    if !canvas.contains(seed.0, seed.1) {
        return mask;
    }
    let target = sample(seed.0, seed.1).to_straight();
    let tol = tolerance.clamp(0.0, 1.0) + 1e-4;
    let matches = |x: i32, y: i32| {
        let c = sample(x, y).to_straight();
        // Treat fully transparent pixels as one colour.
        if target[3] <= 0.001 && c[3] <= 0.001 {
            return true;
        }
        (0..4).all(|i| (c[i] - target[i]).abs() <= tol)
    };
    if !contiguous {
        for y in canvas.y..canvas.bottom() {
            for x in canvas.x..canvas.right() {
                if matches(x, y) {
                    mask.set_value(x, y, 1.0);
                }
            }
        }
        return mask;
    }
    // Scanline flood fill over a visited bitmap.
    let w = canvas.w as usize;
    let mut visited = vec![false; w * canvas.h as usize];
    let idx = |x: i32, y: i32| (y - canvas.y) as usize * w + (x - canvas.x) as usize;
    let mut stack = vec![seed];
    while let Some((sx, sy)) = stack.pop() {
        if visited[idx(sx, sy)] || !matches(sx, sy) {
            continue;
        }
        // Expand left and right along the row.
        let mut x0 = sx;
        while x0 > canvas.x && !visited[idx(x0 - 1, sy)] && matches(x0 - 1, sy) {
            x0 -= 1;
        }
        let mut x1 = sx;
        while x1 + 1 < canvas.right() && !visited[idx(x1 + 1, sy)] && matches(x1 + 1, sy) {
            x1 += 1;
        }
        for x in x0..=x1 {
            visited[idx(x, sy)] = true;
            mask.set_value(x, sy, 1.0);
            for ny in [sy - 1, sy + 1] {
                if ny >= canvas.y && ny < canvas.bottom() && !visited[idx(x, ny)] && matches(x, ny) {
                    stack.push((x, ny));
                }
            }
        }
    }
    mask
}

fn sampler<'a>(
    doc: &'a Document,
    source: SampleSource,
) -> Result<Box<dyn Fn(i32, i32) -> Rgba + 'a>, EditError> {
    match source {
        SampleSource::Merged => {
            let flat = lumenply_render::composite_raster(doc);
            let (w, h) = (flat.width, flat.height);
            Ok(Box::new(move |x, y| {
                if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h {
                    Rgba::TRANSPARENT
                } else {
                    flat.get(x as u32, y as u32)
                }
            }))
        }
        SampleSource::Layer(id) => {
            let store = doc
                .layer(id)
                .ok_or(EditError::NoLayer(id))?
                .pixels()
                .ok_or(EditError::NotPixel(id))?;
            Ok(Box::new(move |x, y| store.get_pixel(x, y)))
        }
    }
}

/// Paint bucket: fill the region around `(x, y)` with a colour.
pub struct BucketFill {
    pub layer: LayerId,
    pub x: i32,
    pub y: i32,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    pub tolerance: f32,
    pub contiguous: bool,
    pub sample: SampleSource,
}

impl Command for BucketFill {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Paint bucket".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let region = {
            let sample = sampler(doc, self.sample)?;
            flood_region(
                &*sample,
                canvas,
                (self.x, self.y),
                self.tolerance,
                self.contiguous,
            )
        };
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let [r, g, b, a] = self.color;
        let area = region.bounds_within(canvas);
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let cov = region.value(px, py) * sel.as_ref().map_or(1.0, |s| s.value(px, py));
                if cov <= 0.0 {
                    continue;
                }
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, Rgba::from_straight(r, g, b, a * cov).over(dst));
            }
        }
        store.prune_blank();
        Ok(())
    }
}

/// Clone stamp: paint pixels copied from `offset` away, sampled from one
/// layer or the merged image as it was before the stroke.
pub struct CloneStroke {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
    /// Source position = destination + offset (in canvas pixels).
    pub offset: (i32, i32),
    pub sample: SampleSource,
}

impl Command for CloneStroke {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Clone stamp".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        // Snapshot the source first so a stroke cannot feed on itself.
        let source: Raster = {
            let sample = sampler(doc, self.sample)?;
            let mut r = Raster::new(canvas.w, canvas.h);
            for y in 0..canvas.h as i32 {
                for x in 0..canvas.w as i32 {
                    r.set(x as u32, y as u32, sample(x, y));
                }
            }
            r
        };
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let strength = self.brush.color[3].clamp(0.0, 1.0);
        let (ox, oy) = self.offset;
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, sel.as_ref(), |px, py, cover| {
                let (sx, sy) = (px + ox, py + oy);
                if !canvas.contains(sx, sy) {
                    return;
                }
                let src = source.get(sx as u32, sy as u32).scale(strength * cover);
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, src.over(dst));
            });
        }
        Ok(())
    }
}

/// Replace the document's work path (the pen tool coalesces its clicks
/// through this, so drawing a path is one undo step).
pub struct SetWorkPath {
    pub path: Option<lumenply_doc::VectorPath>,
}

impl Command for SetWorkPath {
    fn label(&self) -> String {
        match &self.path {
            Some(_) => "Edit path".into(),
            None => "Clear path".into(),
        }
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        // The path is an overlay; pixels don't change.
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        doc.work_path = self.path.clone().filter(|p| !p.subpaths.is_empty());
        Ok(())
    }
}

/// Switch the document's 32-bit float (HDR) mode. On, tiles stop
/// compacting so values outside [0, 1] survive edits; off, tiles return
/// to 16-bit (clamping anything outside the range) lazily, as each one
/// is next edited — tiles shared with history cannot be touched eagerly
/// (ADR 0004).
pub struct SetFloatMode {
    pub on: bool,
}

impl Command for SetFloatMode {
    fn label(&self) -> String {
        if self.on {
            "32-bit float mode on".into()
        } else {
            "32-bit float mode off".into()
        }
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        // The toggle changes storage, not what the composite shows (HDR
        // values already display clamped).
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        doc.float_mode = self.on;
        Ok(())
    }
}

/// Store a copy of the work path in the document's Paths list.
pub struct SaveWorkPath {
    pub name: String,
}

impl Command for SaveWorkPath {
    fn label(&self) -> String {
        format!("Save path \"{}\"", self.name)
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let path = doc
            .work_path
            .clone()
            .ok_or_else(|| EditError::Invalid("there is no path; draw one with the pen first".into()))?;
        if self.name.trim().is_empty() {
            return Err(EditError::Invalid("the path needs a name".into()));
        }
        doc.saved_paths.push(lumenply_doc::NamedPath {
            name: self.name.clone(),
            path,
        });
        Ok(())
    }
}

/// Make a saved path the work path again (a copy; the stored one stays).
pub struct UseSavedPath {
    pub index: usize,
}

impl Command for UseSavedPath {
    fn label(&self) -> String {
        "Load path".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let named = doc
            .saved_paths
            .get(self.index)
            .ok_or_else(|| EditError::Invalid(format!("no saved path #{}", self.index)))?;
        doc.work_path = Some(named.path.clone());
        Ok(())
    }
}

/// Remove a path from the Paths list.
pub struct DeleteSavedPath {
    pub index: usize,
}

impl Command for DeleteSavedPath {
    fn label(&self) -> String {
        "Delete path".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.index >= doc.saved_paths.len() {
            return Err(EditError::Invalid(format!("no saved path #{}", self.index)));
        }
        doc.saved_paths.remove(self.index);
        Ok(())
    }
}

/// Coverage of the work path's closed (or auto-closed) subpaths,
/// antialiased, as a selection-style mask.
fn work_path_coverage(doc: &Document) -> EditResult<Mask> {
    let path = doc
        .work_path
        .as_ref()
        .ok_or_else(|| EditError::Invalid("there is no path; draw one with the pen first".into()))?;
    let mut mask = Mask::hide_all();
    // All subpaths fill as one even-odd region, so a subpath drawn inside
    // another cuts a hole instead of unioning.
    let polys: Vec<Vec<(f32, f32)>> = path
        .flatten()
        .into_iter()
        .map(|(pts, _closed)| pts)
        .filter(|pts| pts.len() >= 3)
        .collect();
    if polys.is_empty() {
        return Err(EditError::Invalid("the path has no fillable subpath".into()));
    }
    mask.fill_polygons(&polys, 1.0);
    Ok(mask)
}

/// Fill the work path's area with a colour on a pixel layer.
pub struct FillPath {
    pub layer: LayerId,
    /// Straight linear RGBA.
    pub color: [f32; 4],
}

impl Command for FillPath {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Fill path".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        doc.work_path.as_ref().map(|p| {
            let mut b: Option<Rect> = None;
            for (pts, _) in p.flatten() {
                for (x, y) in pts {
                    let r = Rect::new(x.floor() as i32 - 1, y.floor() as i32 - 1, 3, 3);
                    b = Some(b.map_or(r, |acc| acc.union(&r)));
                }
            }
            b.unwrap_or_default().intersect(&doc.canvas())
        })
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let coverage = work_path_coverage(doc)?;
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let area = coverage.bounds_within(canvas);
        let [r, g, b, a] = self.color;
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let cov = coverage.value(px, py) * sel.as_ref().map_or(1.0, |s| s.value(px, py));
                if cov <= 0.0 {
                    continue;
                }
                let src = Rgba::from_straight(r, g, b, a * cov);
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, src.over(dst));
            }
        }
        store.prune_blank();
        Ok(())
    }
}

/// Stroke the work path with the brush on a pixel layer.
pub struct StrokeWorkPath {
    pub layer: LayerId,
    pub brush: Brush,
}

impl Command for StrokeWorkPath {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Stroke path".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let path = doc
            .work_path
            .clone()
            .ok_or_else(|| EditError::Invalid("there is no path; draw one with the pen first".into()))?;
        let mut any = false;
        for (pts, closed) in path.flatten() {
            if pts.len() < 2 {
                continue;
            }
            let mut points: Vec<StrokePoint> =
                pts.iter().map(|&(x, y)| StrokePoint::new(x, y, 1.0)).collect();
            if closed {
                points.push(points[0]);
            }
            PaintStroke {
                layer: self.layer,
                brush: self.brush,
                points,
            }
            .apply(doc)?;
            any = true;
        }
        if !any {
            return Err(EditError::Invalid("the path has no strokeable subpath".into()));
        }
        Ok(())
    }
}

/// Turn the work path into the selection.
pub struct PathToSelection {
    pub op: CombineOp,
}

impl Command for PathToSelection {
    fn label(&self) -> String {
        "Selection from path".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let coverage = work_path_coverage(doc)?;
        let new = Selection { coverage };
        let mut sel = doc.selection.take().unwrap_or_else(Selection::none);
        sel.combine(&new, self.op);
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

/// Heal a stroke: repaint each dab with colour diffused in from just
/// outside it (a small Jacobi solve), so blemishes melt into their
/// surroundings — including across gradients, where cloning would seam.
/// With `texture` set, the high-frequency detail of a clone-style source
/// (destination + `offset`) is layered back on top, which is the classic
/// healing brush; without it this is spot healing.
pub struct HealStroke {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
    /// Source position = destination + offset (texture mode only).
    pub offset: (i32, i32),
    pub sample: SampleSource,
    pub texture: bool,
}

impl Command for HealStroke {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.texture {
            "Healing brush".into()
        } else {
            "Spot heal".into()
        }
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let source: Option<Raster> = if self.texture {
            let sample = sampler(doc, self.sample)?;
            let mut r = Raster::new(canvas.w, canvas.h);
            for y in 0..canvas.h as i32 {
                for x in 0..canvas.w as i32 {
                    r.set(x as u32, y as u32, sample(x, y));
                }
            }
            Some(r)
        } else {
            None
        };
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let strength = self.brush.color[3].clamp(0.0, 1.0);
        for d in interpolate_dabs(&self.brush, &self.points) {
            heal_dab(
                store,
                &self.brush,
                d,
                canvas,
                sel.as_ref(),
                source.as_ref(),
                self.offset,
                strength,
            );
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn heal_dab(
    store: &mut lumenply_tiles::TileStore,
    brush: &Brush,
    p: StrokePoint,
    canvas: Rect,
    sel: Option<&Selection>,
    source: Option<&Raster>,
    offset: (i32, i32),
    strength: f32,
) {
    let r = brush.radius * p.pressure.clamp(0.0, 1.0);
    if r <= 0.0 {
        return;
    }
    let pad = r.ceil() as i32 + 2;
    let region = Rect::new(
        p.x as i32 - pad,
        p.y as i32 - pad,
        (2 * pad) as u32 + 1,
        (2 * pad) as u32 + 1,
    )
    .intersect(&canvas);
    if region.is_empty() {
        return;
    }
    let (w, h) = (region.w as usize, region.h as usize);

    // Straight-colour grid of the destination; alpha 0 means "no data".
    let mut rgb = vec![[0f32; 3]; w * h];
    let mut alpha = vec![0f32; w * h];
    let mut cover = vec![0f32; w * h];
    for gy in 0..h {
        for gx in 0..w {
            let (x, y) = (region.x + gx as i32, region.y + gy as i32);
            let px = store.get_pixel(x, y);
            let [cr, cg, cb, a] = px.to_straight();
            rgb[gy * w + gx] = [cr, cg, cb];
            alpha[gy * w + gx] = a;
        }
    }
    dab_coverage(brush, p, canvas, sel, |x, y, c| {
        cover[(y - region.y) as usize * w + (x - region.x) as usize] = c;
    });

    // Diffuse the surround into the dab: unknowns start at the mean of the
    // known rim and relax toward their neighbours (Jacobi).
    let unknown: Vec<bool> = cover.iter().map(|&c| c > 0.01).collect();
    let mut known_sum = [0f32; 3];
    let mut known_n = 0f32;
    for i in 0..w * h {
        if !unknown[i] && alpha[i] > 0.0 {
            for k in 0..3 {
                known_sum[k] += rgb[i][k];
            }
            known_n += 1.0;
        }
    }
    if known_n <= 0.0 {
        return; // nothing to heal from
    }
    let mean = [
        known_sum[0] / known_n,
        known_sum[1] / known_n,
        known_sum[2] / known_n,
    ];
    let mut field: Vec<[f32; 3]> = (0..w * h)
        .map(|i| if unknown[i] { mean } else { rgb[i] })
        .collect();
    let mut next = field.clone();
    let iters = (2 * (w.max(h)) as u32).clamp(20, 120);
    for _ in 0..iters {
        for gy in 0..h {
            for gx in 0..w {
                let i = gy * w + gx;
                if !unknown[i] {
                    continue;
                }
                let mut acc = [0f32; 3];
                let mut n = 0f32;
                let mut push = |j: usize| {
                    for k in 0..3 {
                        acc[k] += field[j][k];
                    }
                    n += 1.0;
                };
                if gx > 0 {
                    push(i - 1);
                }
                if gx + 1 < w {
                    push(i + 1);
                }
                if gy > 0 {
                    push(i - w);
                }
                if gy + 1 < h {
                    push(i + w);
                }
                if n > 0.0 {
                    next[i] = [acc[0] / n, acc[1] / n, acc[2] / n];
                }
            }
        }
        std::mem::swap(&mut field, &mut next);
    }

    // Texture: add back the source patch's high-frequency detail.
    let highfreq: Option<Vec<[f32; 3]>> = source.map(|src| {
        let mut patch = vec![[0f32; 3]; w * h];
        for gy in 0..h {
            for gx in 0..w {
                let (sx, sy) = (
                    (region.x + gx as i32 + offset.0).clamp(canvas.x, canvas.right() - 1),
                    (region.y + gy as i32 + offset.1).clamp(canvas.y, canvas.bottom() - 1),
                );
                let [cr, cg, cb, _] = src.get(sx as u32, sy as u32).to_straight();
                patch[gy * w + gx] = [cr, cg, cb];
            }
        }
        let low = box_blur_rgb(&patch, w, h, ((r / 2.0) as usize).max(2));
        patch
            .iter()
            .zip(low.iter())
            .map(|(p, l)| [p[0] - l[0], p[1] - l[1], p[2] - l[2]])
            .collect()
    });

    for gy in 0..h {
        for gx in 0..w {
            let i = gy * w + gx;
            let k = (cover[i] * strength).clamp(0.0, 1.0);
            if k <= 0.0 || alpha[i] <= 0.0 {
                continue;
            }
            let mut out = field[i];
            if let Some(hf) = &highfreq {
                for c in 0..3 {
                    out[c] = (out[c] + hf[i][c]).clamp(0.0, 1.0);
                }
            }
            let cur = rgb[i];
            let healed = [
                cur[0] + (out[0] - cur[0]) * k,
                cur[1] + (out[1] - cur[1]) * k,
                cur[2] + (out[2] - cur[2]) * k,
            ];
            store.set_pixel(
                region.x + gx as i32,
                region.y + gy as i32,
                Rgba::from_straight(healed[0], healed[1], healed[2], alpha[i]),
            );
        }
    }
}

/// Simple separable box blur over a straight-RGB grid (edge-clamped).
fn box_blur_rgb(src: &[[f32; 3]], w: usize, h: usize, radius: usize) -> Vec<[f32; 3]> {
    let r = radius as i32;
    let mut tmp = vec![[0f32; 3]; w * h];
    let mut out = vec![[0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for dx in -r..=r {
                let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                for k in 0..3 {
                    acc[k] += src[y * w + sx][k];
                }
            }
            let n = (2 * r + 1) as f32;
            tmp[y * w + x] = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for dy in -r..=r {
                let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                for k in 0..3 {
                    acc[k] += tmp[sy * w + x][k];
                }
            }
            let n = (2 * r + 1) as f32;
            out[y * w + x] = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    }
    out
}

/// Rotate the whole image by quarter turns (positive = clockwise) or flip it.
pub struct RotateImage {
    pub quarter_turns: i32,
}

impl Command for RotateImage {
    fn label(&self) -> String {
        match self.quarter_turns.rem_euclid(4) {
            1 => "Rotate 90° clockwise".into(),
            2 => "Rotate 180°".into(),
            3 => "Rotate 90° counter-clockwise".into(),
            _ => "Rotate".into(),
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let q = self.quarter_turns.rem_euclid(4);
        if q == 0 {
            return Ok(());
        }
        let (w, h) = (doc.width as f32, doc.height as f32);
        // Rotate about the origin, then translate back onto the new canvas.
        let angle = std::f32::consts::FRAC_PI_2 * q as f32;
        let (tx, ty) = match q {
            1 => (h, 0.0),
            2 => (w, h),
            _ => (0.0, w),
        };
        let t = Affine::rotate(angle).then(&Affine::translate(tx, ty));
        doc.for_each_layer_mut(|l| {
            match &mut l.content {
                LayerContent::Pixel(store) => *store = lumenply_render::transform_store(store, &t),
                LayerContent::Smart(sm) => smart_compose(sm, &t),
                LayerContent::Text(tl) => {
                    let (nx, ny) = t.apply(tl.x, tl.y);
                    tl.x = nx;
                    tl.y = ny;
                    lumenply_render::text::refresh_cache(tl);
                }
                _ => {}
            }
            if let Some(m) = l.mask.as_mut() {
                *m = lumenply_render::transform_mask(m, &t);
            }
        });
        if q % 2 == 1 {
            std::mem::swap(&mut doc.width, &mut doc.height);
        }
        doc.selection = None;
        Ok(())
    }
}

/// Mirror the whole image.
pub struct FlipImage {
    pub horizontal: bool,
}

impl Command for FlipImage {
    fn label(&self) -> String {
        if self.horizontal {
            "Flip image horizontal"
        } else {
            "Flip image vertical"
        }
        .into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let (w, h) = (doc.width as f32, doc.height as f32);
        let t = if self.horizontal {
            Affine::scale(-1.0, 1.0).then(&Affine::translate(w, 0.0))
        } else {
            Affine::scale(1.0, -1.0).then(&Affine::translate(0.0, h))
        };
        doc.for_each_layer_mut(|l| {
            match &mut l.content {
                LayerContent::Pixel(store) => *store = lumenply_render::transform_store(store, &t),
                LayerContent::Smart(sm) => smart_compose(sm, &t),
                LayerContent::Text(tl) => {
                    let (nx, ny) = t.apply(tl.x, tl.y);
                    tl.x = nx;
                    tl.y = ny;
                    lumenply_render::text::refresh_cache(tl);
                }
                _ => {}
            }
            if let Some(m) = l.mask.as_mut() {
                *m = lumenply_render::transform_mask(m, &t);
            }
        });
        doc.selection = None;
        Ok(())
    }
}

/// Magic wand: select the region around `(x, y)`.
pub struct MagicWandSelect {
    pub x: i32,
    pub y: i32,
    pub tolerance: f32,
    pub contiguous: bool,
    pub sample: SampleSource,
    pub op: CombineOp,
}

impl Command for MagicWandSelect {
    fn label(&self) -> String {
        "Magic wand".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let region = {
            let sample = sampler(doc, self.sample)?;
            flood_region(
                &*sample,
                canvas,
                (self.x, self.y),
                self.tolerance,
                self.contiguous,
            )
        };
        let shape = Selection::from_mask(&region);
        let mut sel = doc.selection.take().unwrap_or_else(Selection::none);
        sel.combine(&shape, self.op);
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientKind {
    Linear,
    Radial,
}

/// Fill the selection (or canvas) with a two-colour gradient, composited
/// over the existing pixels. Colours interpolate in linear light.
pub struct GradientFill {
    pub layer: LayerId,
    pub start: (f32, f32),
    pub end: (f32, f32),
    /// Straight linear RGBA at the start and at the end.
    pub colors: [[f32; 4]; 2],
    pub kind: GradientKind,
}

impl Command for GradientFill {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Gradient".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(
            doc.selection
                .as_ref()
                .map_or(doc.canvas(), |s| s.bounds_within(doc.canvas())),
        )
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let area = sel.as_ref().map_or(canvas, |s| s.bounds_within(canvas));
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let (sx, sy) = self.start;
        let (ex, ey) = self.end;
        let (dx, dy) = (ex - sx, ey - sy);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let [c0, c1] = self.colors;
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let cov = sel.as_ref().map_or(1.0, |s| s.value(px, py));
                if cov <= 0.0 {
                    continue;
                }
                let (fx, fy) = (px as f32 + 0.5 - sx, py as f32 + 0.5 - sy);
                let t = match self.kind {
                    GradientKind::Linear => (fx * dx + fy * dy) / len2,
                    GradientKind::Radial => ((fx * fx + fy * fy) / len2).sqrt(),
                }
                .clamp(0.0, 1.0);
                let mix = |i: usize| c0[i] + (c1[i] - c0[i]) * t;
                let src = Rgba::from_straight(mix(0), mix(1), mix(2), mix(3) * cov);
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, src.over(dst));
            }
        }
        store.prune_blank();
        Ok(())
    }
}

pub struct RemoveLayer {
    pub layer: LayerId,
}

impl Command for RemoveLayer {
    fn label(&self) -> String {
        "Remove layer".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        doc.remove_layer(self.layer)
            .map(|_| ())
            .ok_or(EditError::NoLayer(self.layer))
    }
}

pub struct SetOpacity {
    pub layer: LayerId,
    pub opacity: f32,
}

impl Command for SetOpacity {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Set opacity".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.opacity = self.opacity.clamp(0.0, 1.0);
        Ok(())
    }
}

pub struct SetBlendMode {
    pub layer: LayerId,
    pub blend: BlendMode,
}

impl Command for SetBlendMode {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        format!("Blend mode: {}", self.blend.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.blend = self.blend;
        Ok(())
    }
}

pub struct SetVisible {
    pub layer: LayerId,
    pub visible: bool,
}

impl Command for SetVisible {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        if self.visible { "Show layer" } else { "Hide layer" }.into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.visible = self.visible;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrushMode {
    /// Lay colour down over what is there.
    #[default]
    Paint,
    /// Remove coverage (the eraser); `color[3]` acts as strength.
    Erase,
    /// Lighten what is there (gamma-domain, like the classic tool);
    /// `color[3]` acts as strength.
    Dodge,
    /// Darken what is there; `color[3]` acts as strength.
    Burn,
    /// Drag pixels along the stroke; `color[3]` acts as strength.
    Smudge,
    /// Boost saturation (the sponge); `color[3]` acts as strength.
    Saturate,
    /// Drain saturation; `color[3]` acts as strength.
    Desaturate,
}

/// A round brush. Pressure scales the radius; `hardness` 1.0 is a crisp
/// (antialiased) edge, 0.0 a fully soft falloff.
#[derive(Clone, Copy, Debug)]
pub struct Brush {
    pub radius: f32,
    pub hardness: f32,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    /// Distance between dabs as a fraction of the radius.
    pub spacing: f32,
    /// Scatter: each dab lands up to `jitter × radius` off the stroke, in
    /// a direction and distance hashed from its index, so replays are
    /// deterministic. 0 keeps dabs on the line.
    pub jitter: f32,
    pub mode: BrushMode,
}

impl Default for Brush {
    fn default() -> Self {
        Brush {
            radius: 8.0,
            hardness: 0.8,
            color: [0.0, 0.0, 0.0, 1.0],
            spacing: 0.2,
            jitter: 0.0,
            mode: BrushMode::Paint,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

impl StrokePoint {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        StrokePoint { x, y, pressure }
    }
}

/// Paint a polyline of dabs onto a pixel layer.
pub struct PaintStroke {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
}

/// Bounding box of a stroke's dabs, grown by the brush radius.
pub fn stroke_bounds(brush: &Brush, points: &[StrokePoint], canvas: Rect) -> Rect {
    let r = (brush.radius * (1.0 + brush.jitter.clamp(0.0, 1.0))).ceil() as i32 + 2;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in points {
        x0 = x0.min(p.x.floor() as i32 - r);
        y0 = y0.min(p.y.floor() as i32 - r);
        x1 = x1.max(p.x.ceil() as i32 + r);
        y1 = y1.max(p.y.ceil() as i32 + r);
    }
    if x1 < x0 {
        return Rect::default();
    }
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&canvas)
}

impl Command for PaintStroke {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        match self.brush.mode {
            BrushMode::Paint => "Paint stroke".into(),
            BrushMode::Erase => "Erase".into(),
            BrushMode::Dodge => "Dodge".into(),
            BrushMode::Burn => "Burn".into(),
            BrushMode::Smudge => "Smudge".into(),
            BrushMode::Saturate => "Sponge (saturate)".into(),
            BrushMode::Desaturate => "Sponge (desaturate)".into(),
        }
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let [cr, cg, cb, ca] = self.brush.color;
        let mode = self.brush.mode;
        if mode == BrushMode::Smudge {
            // Each dab stamps the pixels from under the previous dab,
            // sampled from a snapshot so a dab never reads its own writes.
            let mut prev: Option<StrokePoint> = None;
            for d in interpolate_dabs(&self.brush, &self.points) {
                if let Some(p) = prev {
                    let (ox, oy) = ((d.x - p.x).round() as i32, (d.y - p.y).round() as i32);
                    if ox != 0 || oy != 0 {
                        let r = (self.brush.radius.ceil() as i32 + 2).max(1);
                        let (sx, sy) = (d.x.round() as i32 - ox - r, d.y.round() as i32 - oy - r);
                        let side = (2 * r + 1) as usize;
                        let mut snap = vec![Rgba::TRANSPARENT; side * side];
                        for (i, q) in snap.iter_mut().enumerate() {
                            let (gx, gy) = (sx + (i % side) as i32, sy + (i / side) as i32);
                            *q = store.get_pixel(gx, gy);
                        }
                        dab_coverage(&self.brush, d, canvas, sel.as_ref(), |px, py, cover| {
                            let (lx, ly) = (px - ox - sx, py - oy - sy);
                            if lx < 0 || ly < 0 || lx >= side as i32 || ly >= side as i32 {
                                return;
                            }
                            let src = snap[ly as usize * side + lx as usize];
                            let dst = store.get_pixel(px, py);
                            let k = (ca * cover).clamp(0.0, 1.0);
                            // Premultiplied pixels lerp component-wise.
                            let mix = Rgba::new(
                                dst.r + (src.r - dst.r) * k,
                                dst.g + (src.g - dst.g) * k,
                                dst.b + (src.b - dst.b) * k,
                                dst.a + (src.a - dst.a) * k,
                            );
                            store.set_pixel(px, py, mix);
                        });
                    }
                }
                prev = Some(d);
            }
            return Ok(());
        }
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, sel.as_ref(), |px, py, cover| {
                let dst = store.get_pixel(px, py);
                let out = match mode {
                    BrushMode::Paint => Rgba::from_straight(cr, cg, cb, ca * cover).over(dst),
                    BrushMode::Erase => dst.scale(1.0 - (ca * cover).clamp(0.0, 1.0)),
                    BrushMode::Dodge | BrushMode::Burn => {
                        if dst.a <= 0.0 {
                            return;
                        }
                        let k = (ca * cover).clamp(0.0, 1.0);
                        let [r, g, b, a] = dst.to_straight();
                        let tone = |c: f32| {
                            let e = lumenply_doc::adjust::srgb_encode(c);
                            let e = if mode == BrushMode::Dodge {
                                e + (1.0 - e) * k
                            } else {
                                e * (1.0 - k)
                            };
                            lumenply_doc::adjust::srgb_decode(e)
                        };
                        Rgba::from_straight(tone(r), tone(g), tone(b), a)
                    }
                    BrushMode::Saturate | BrushMode::Desaturate => {
                        if dst.a <= 0.0 {
                            return;
                        }
                        let k = (ca * cover).clamp(0.0, 1.0);
                        // Scale chroma around the gamma-domain luminance,
                        // like the sponge tool's perceptual behaviour.
                        let scale = if mode == BrushMode::Saturate {
                            1.0 + k
                        } else {
                            1.0 - k
                        };
                        let [r, g, b, a] = dst.to_straight();
                        let enc = lumenply_doc::adjust::srgb_encode;
                        let dec = lumenply_doc::adjust::srgb_decode;
                        let (er, eg, eb) = (enc(r), enc(g), enc(b));
                        let y = 0.2126 * er + 0.7152 * eg + 0.0722 * eb;
                        let sat = |c: f32| dec((y + (c - y) * scale).clamp(0.0, 1.0));
                        Rgba::from_straight(sat(er), sat(eg), sat(eb), a)
                    }
                    BrushMode::Smudge => unreachable!("handled above"),
                };
                store.set_pixel(px, py, out);
            });
        }
        Ok(())
    }
}

/// Paint onto a layer's mask. The brush colour's luminance is the target
/// coverage (white reveals, black hides); erase mode always hides.
pub struct PaintMask {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
}

impl Command for PaintMask {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Paint mask".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let mask = layer
            .mask
            .as_mut()
            .ok_or_else(|| EditError::Invalid(format!("layer {} has no mask", self.layer)))?;
        let [cr, cg, cb, ca] = self.brush.color;
        let target = match self.brush.mode {
            BrushMode::Erase | BrushMode::Burn | BrushMode::Desaturate => 0.0,
            BrushMode::Dodge | BrushMode::Saturate => 1.0,
            // Paint (and modes with no mask meaning) write the colour's
            // luminance: white reveals, black hides.
            _ => lumenply_doc::adjust::luminance(cr, cg, cb).clamp(0.0, 1.0),
        };
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, sel.as_ref(), |px, py, cover| {
                let k = (ca * cover).clamp(0.0, 1.0);
                let v = mask.value(px, py);
                mask.set_value(px, py, v + (target - v) * k);
            });
        }
        Ok(())
    }
}

/// Smooth sparse input points with a Catmull-Rom spline so strokes drawn
/// with a mouse (few events per frame) don't look like polylines. Input with
/// fewer than three points is returned unchanged.
pub fn smooth_stroke(points: &[StrokePoint]) -> Vec<StrokePoint> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(points.len() * 6);
    let get = |i: isize| points[i.clamp(0, points.len() as isize - 1) as usize];
    for i in 0..points.len() - 1 {
        let (p0, p1, p2, p3) = (
            get(i as isize - 1),
            get(i as isize),
            get(i as isize + 1),
            get(i as isize + 2),
        );
        let seg_len = ((p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2)).sqrt();
        let steps = (seg_len / 3.0).ceil().clamp(1.0, 24.0) as usize;
        for k in 0..steps {
            let t = k as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let cr = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * ((2.0 * b)
                    + (-a + c) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            out.push(StrokePoint::new(
                cr(p0.x, p1.x, p2.x, p3.x),
                cr(p0.y, p1.y, p2.y, p3.y),
                p1.pressure + (p2.pressure - p1.pressure) * t,
            ));
        }
    }
    out.push(*points.last().expect("non-empty"));
    out
}

/// Dab centres along a polyline, spaced by the pressure-scaled radius so
/// thin stroke ends stay continuous.
fn interpolate_dabs(brush: &Brush, points: &[StrokePoint]) -> Vec<StrokePoint> {
    let smoothed = smooth_stroke(points);
    let points = &smoothed[..];
    let mut dabs = vec![points[0]];
    let mut carry = 0.0f32;
    let jitter = brush.jitter.clamp(0.0, 1.0);
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if len <= 0.0 {
            continue;
        }
        let eff = brush.radius * (0.5 * (a.pressure + b.pressure)).clamp(0.0, 1.0);
        let step = (brush.spacing * eff).max(0.5);
        let mut t = step - carry;
        while t <= len {
            let f = t / len;
            dabs.push(StrokePoint::new(
                a.x + (b.x - a.x) * f,
                a.y + (b.y - a.y) * f,
                a.pressure + (b.pressure - a.pressure) * f,
            ));
            t += step;
        }
        carry = len - (t - step);
    }
    if jitter > 0.0 {
        // Scatter each dab by a hash of its index: deterministic, so undo
        // previews and replays stamp identical pixels.
        for (i, d) in dabs.iter_mut().enumerate() {
            let angle = hash01(i as u32, 0x9E37_79B9) * std::f32::consts::TAU;
            let dist = hash01(i as u32, 0x85EB_CA6B).sqrt() * jitter * brush.radius;
            d.x += angle.cos() * dist;
            d.y += angle.sin() * dist;
        }
    }
    dabs
}

/// Deterministic hash of (n, salt) onto [0, 1).
fn hash01(n: u32, salt: u32) -> f32 {
    let mut h = n.wrapping_mul(0x27D4_EB2F) ^ salt;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Call `f(x, y, coverage)` for every canvas pixel a dab touches, with the
/// selection already applied to the coverage.
fn dab_coverage(
    brush: &Brush,
    p: StrokePoint,
    canvas: Rect,
    sel: Option<&Selection>,
    mut f: impl FnMut(i32, i32, f32),
) {
    let r = brush.radius * p.pressure.clamp(0.0, 1.0);
    if r <= 0.0 {
        return;
    }
    let x0 = (p.x - r).floor() as i32;
    let y0 = (p.y - r).floor() as i32;
    let x1 = (p.x + r).ceil() as i32;
    let y1 = (p.y + r).ceil() as i32;
    let area = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32).intersect(&canvas);
    let hard = brush.hardness.clamp(0.0, 0.999);
    for py in area.y..area.bottom() {
        for px in area.x..area.right() {
            let dx = px as f32 + 0.5 - p.x;
            let dy = py as f32 + 0.5 - p.y;
            let d = (dx * dx + dy * dy).sqrt() / r;
            let edge = ((1.0 - d) * r + 0.5).clamp(0.0, 1.0);
            let soft = if d <= hard {
                1.0
            } else {
                1.0 - (d - hard) / (1.0 - hard)
            };
            let cover = edge * soft.clamp(0.0, 1.0) * sel.map_or(1.0, |s| s.value(px, py));
            if cover > 0.0 {
                f(px, py, cover);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_covers_its_path_and_respects_canvas() {
        let mut doc = Document::new(100, 50);
        let id = doc.add_pixel_layer("p");
        let stroke = PaintStroke {
            layer: id,
            brush: Brush {
                radius: 3.0,
                hardness: 1.0,
                color: [0.0, 0.0, 1.0, 1.0],
                spacing: 0.3,
                jitter: 0.0,
                mode: BrushMode::Paint,
            },
            points: vec![
                StrokePoint::new(5.0, 25.0, 1.0),
                StrokePoint::new(95.0, 25.0, 1.0),
            ],
        };
        stroke.apply(&mut doc).unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        assert!(px.get_pixel(50, 25).a > 0.99);
        assert!(px.get_pixel(50, 24).a > 0.99);
        assert_eq!(px.get_pixel(50, 40), Rgba::TRANSPARENT);
        // Nothing is painted outside the canvas (and no tiles are allocated there).
        assert!(px.bounds().unwrap().x >= 0);
    }

    #[test]
    fn fill_clear_and_paint_respect_the_selection() {
        let mut doc = Document::new(100, 100);
        let id = doc.add_pixel_layer("p");
        SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 10, 20, 20))),
        }
        .apply(&mut doc)
        .unwrap();
        Fill {
            layer: id,
            color: [1.0, 0.0, 0.0, 1.0],
        }
        .apply(&mut doc)
        .unwrap();
        let px = |doc: &Document, x, y| doc.layer(id).unwrap().pixels().unwrap().get_pixel(x, y);
        assert!(px(&doc, 15, 15).a > 0.99);
        assert_eq!(px(&doc, 50, 50), Rgba::TRANSPARENT);

        // Painting outside the selection does nothing.
        PaintStroke {
            layer: id,
            brush: Brush::default(),
            points: vec![StrokePoint::new(60.0, 60.0, 1.0)],
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(px(&doc, 60, 60), Rgba::TRANSPARENT);

        // Clearing a smaller selection punches a hole.
        ModifySelection {
            shape: Selection::rect(Rect::new(12, 12, 4, 4)),
            op: CombineOp::Replace,
        }
        .apply(&mut doc)
        .unwrap();
        Clear { layer: id }.apply(&mut doc).unwrap();
        assert_eq!(px(&doc, 13, 13), Rgba::TRANSPARENT);
        assert!(px(&doc, 20, 20).a > 0.99);

        // Mask from selection, then deselect.
        MaskFromSelection { layer: id }.apply(&mut doc).unwrap();
        assert_eq!(doc.layer(id).unwrap().mask.as_ref().unwrap().value(13, 13), 1.0);
        SetSelection { selection: None }.apply(&mut doc).unwrap();
        assert!(doc.selection.is_none());

        // With no selection, Clear wipes the layer.
        Clear { layer: id }.apply(&mut doc).unwrap();
        assert!(doc.layer(id).unwrap().pixels().unwrap().is_empty());
    }

    #[test]
    fn selection_ops_are_undoable_and_empty_results_deselect() {
        let mut doc = Document::new(50, 50);
        ModifySelection {
            shape: Selection::rect(Rect::new(0, 0, 10, 10)),
            op: CombineOp::Union,
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.selection.is_some());
        ModifySelection {
            shape: Selection::rect(Rect::new(0, 0, 10, 10)),
            op: CombineOp::Subtract,
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.selection.is_none(), "subtracting everything deselects");
        InvertSelection.apply(&mut doc).unwrap();
        assert_eq!(doc.selection.as_ref().unwrap().value(25, 25), 1.0);
        assert!(FeatherSelection { radius: 2.0 }.apply(&mut doc).is_ok());
    }

    #[test]
    fn eraser_removes_coverage() {
        let mut doc = Document::new(50, 50);
        let id = doc.add_pixel_layer("p");
        Fill {
            layer: id,
            color: [0.0, 1.0, 0.0, 1.0],
        }
        .apply(&mut doc)
        .unwrap();
        let mut brush = Brush {
            radius: 5.0,
            hardness: 1.0,
            ..Brush::default()
        };
        brush.mode = BrushMode::Erase;
        PaintStroke {
            layer: id,
            brush,
            points: vec![StrokePoint::new(25.0, 25.0, 1.0)],
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.get_pixel(25, 25), Rgba::TRANSPARENT);
        assert!(px.get_pixel(5, 5).a > 0.99);
    }

    #[test]
    fn moving_a_layer_keeps_hidden_mask_regions_hidden() {
        // A reveal-all mask with a painted-hidden block. Moving or rotating
        // the layer must keep that block hidden and the rest revealed; the
        // old code pushed the mask through the pixel transform, which pruned
        // all-zero tiles and reset everything outside them to 0.
        let mut doc = Document::new(600, 600);
        let id = doc.add_pixel_layer("a");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(300, 300, Rgba::WHITE);
        let mut mask = Mask::reveal_all();
        mask.fill_rect(Rect::new(0, 0, 256, 256), 0.0); // exactly one all-zero tile
        doc.layer_mut(id).unwrap().mask = Some(mask);

        MoveLayer {
            layer: id,
            dx: 30,
            dy: 7,
        }
        .apply(&mut doc)
        .unwrap();
        let m = doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(m.default, 1.0);
        assert_eq!(m.value(130, 130), 0.0, "hidden block moved, still hidden");
        assert_eq!(m.value(10, 2), 1.0, "uncovered strip reverts to default");
        assert_eq!(m.value(500, 500), 1.0, "far away stays revealed");

        RotateImage { quarter_turns: 1 }.apply(&mut doc).unwrap();
        let m = doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(m.value(450, 130), 0.0, "hidden block rotated with the image");
        assert_eq!(m.value(100, 500), 1.0, "rest of the canvas stays revealed");
    }

    #[test]
    fn relocate_layer_moves_within_and_across_groups() {
        let mut doc = Document::new(8, 8);
        let a = doc.add_pixel_layer("a");
        let b = doc.add_pixel_layer("b");
        let g = doc.add_group("g");
        let inner_id = doc.alloc_id();
        doc.layer_mut(g)
            .unwrap()
            .children_mut()
            .unwrap()
            .push(Layer::pixel(inner_id, "inner"));

        // Root reorder: move `b` to the bottom.
        RelocateLayer {
            layer: b,
            parent: None,
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();
        let order: Vec<&str> = doc.layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(order, ["b", "a", "g"]);

        // Into a group, on top of its children.
        RelocateLayer {
            layer: a,
            parent: Some(g),
            index: 99,
        }
        .apply(&mut doc)
        .unwrap();
        let kids: Vec<&str> = doc
            .layer(g)
            .unwrap()
            .children()
            .unwrap()
            .iter()
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(kids, ["inner", "a"]);
        assert_eq!(doc.layer_count(), 4);

        // Out of the group, to the root bottom.
        RelocateLayer {
            layer: inner_id,
            parent: None,
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers()[0].name, "inner");

        // A group cannot be dropped into its own subtree.
        assert!(matches!(
            RelocateLayer {
                layer: g,
                parent: Some(g),
                index: 0,
            }
            .apply(&mut doc),
            Err(EditError::Invalid(_))
        ));
        assert_eq!(doc.layer_count(), 4, "nothing lost by the refusal");

        // A missing layer or non-group parent errors cleanly.
        assert!(RelocateLayer {
            layer: 999,
            parent: None,
            index: 0
        }
        .apply(&mut doc)
        .is_err());
        assert!(matches!(
            RelocateLayer {
                layer: b,
                parent: Some(inner_id),
                index: 0
            }
            .apply(&mut doc),
            Err(EditError::NotGroup(_))
        ));
        assert_eq!(doc.layer_count(), 4, "failed insert restored the layer");
        assert!(doc.layer(b).is_some());
    }

    #[test]
    fn new_layer_from_selection_copies_covered_pixels_above() {
        let mut doc = Document::new(100, 100);
        let id = doc.add_pixel_layer("src");
        for x in 0..100 {
            doc.layer_mut(id)
                .unwrap()
                .pixels_mut()
                .unwrap()
                .set_pixel(x, 50, Rgba::WHITE);
        }
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 40, 100)));
        NewLayerFromSelection {
            layer: id,
            name: "src copy".into(),
        }
        .apply(&mut doc)
        .unwrap();

        assert_eq!(doc.layer_count(), 2);
        let copy = &doc.layers()[1];
        assert_eq!(copy.name, "src copy");
        let px = copy.pixels().unwrap();
        assert_eq!(px.get_pixel(20, 50), Rgba::WHITE, "inside the selection");
        assert!(px.get_pixel(60, 50).is_transparent(), "outside left behind");
        // The source is untouched.
        let src = doc.layers()[0].pixels().unwrap();
        assert_eq!(src.get_pixel(60, 50), Rgba::WHITE);

        // Without a selection the whole layer is copied.
        doc.selection = None;
        NewLayerFromSelection {
            layer: id,
            name: "full copy".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            doc.layers()[1].name,
            "full copy",
            "inserted directly above the source"
        );
        assert_eq!(doc.layers()[1].pixels().unwrap().get_pixel(60, 50), Rgba::WHITE);

        // Feathered coverage scales alpha.
        let mut sel = Selection::rect(Rect::new(0, 40, 100, 20));
        sel.feather(4.0);
        doc.selection = Some(sel);
        NewLayerFromSelection {
            layer: id,
            name: "soft".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let soft = doc.layers()[1].pixels().unwrap();
        let a = soft.get_pixel(20, 50).a;
        assert!(a > 0.9, "centre nearly opaque: {a}");
    }

    #[test]
    fn move_transform_flip_reorder_rename() {
        let mut doc = Document::new(100, 100);
        let a = doc.add_pixel_layer("a");
        let b = doc.add_pixel_layer("b");
        let c = doc.add_pixel_layer("c");
        doc.layer_mut(b)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(10, 10, Rgba::WHITE);
        let mut mask = Mask::reveal_all();
        mask.set_value(10, 10, 0.5);
        doc.layer_mut(b).unwrap().mask = Some(mask);

        MoveLayer {
            layer: b,
            dx: 5,
            dy: -3,
        }
        .apply(&mut doc)
        .unwrap();
        let l = doc.layer(b).unwrap();
        assert_eq!(l.pixels().unwrap().get_pixel(15, 7), Rgba::WHITE);
        assert_eq!(
            l.mask.as_ref().unwrap().value(15, 7),
            0.5,
            "mask moves with pixels"
        );

        FlipLayer {
            layer: b,
            horizontal: true,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            doc.layer(b).unwrap().pixels().unwrap().get_pixel(15, 7),
            Rgba::WHITE,
            "single pixel flips onto itself"
        );

        let t = TransformLayer::around_center(&doc, b, 2.0, 2.0, 0.0).unwrap();
        t.apply(&mut doc).unwrap();
        let px = doc.layer(b).unwrap().pixels().unwrap();
        assert!(px.bounds().is_some());
        let total: f32 = (0..100)
            .flat_map(|y| (0..100).map(move |x| (x, y)))
            .map(|(x, y)| px.get_pixel(x, y).a)
            .sum();
        assert!(
            (total - 4.0).abs() < 0.3,
            "2× scale covers ~4 pixels, got {total}"
        );

        assert!(TransformLayer {
            layer: a,
            transform: Affine::scale(0.0, 1.0)
        }
        .apply(&mut doc)
        .is_err());

        ReorderLayer { layer: a, delta: 2 }.apply(&mut doc).unwrap();
        let order: Vec<LayerId> = doc.layers().iter().map(|l| l.id).collect();
        assert_eq!(order, vec![b, c, a]);
        ReorderLayer { layer: a, delta: 5 }.apply(&mut doc).unwrap();
        assert_eq!(doc.layers().last().unwrap().id, a, "clamped at the top");

        RenameLayer {
            layer: c,
            name: "  Sky  ".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layer(c).unwrap().name, "Sky");
    }

    #[test]
    fn mask_painting_reveals_and_hides() {
        let mut doc = Document::new(60, 60);
        let id = doc.add_pixel_layer("p");
        Fill {
            layer: id,
            color: [1.0, 0.0, 0.0, 1.0],
        }
        .apply(&mut doc)
        .unwrap();
        AddMask { layer: id }.apply(&mut doc).unwrap();
        assert_eq!(doc.layer(id).unwrap().mask.as_ref().unwrap().value(30, 30), 1.0);
        let black = Brush {
            radius: 5.0,
            hardness: 1.0,
            color: [0.0, 0.0, 0.0, 1.0],
            ..Brush::default()
        };
        PaintMask {
            layer: id,
            brush: black,
            points: vec![StrokePoint::new(30.0, 30.0, 1.0)],
        }
        .apply(&mut doc)
        .unwrap();
        let m = doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(m.value(30, 30) < 0.01, "black hides");
        assert_eq!(m.value(5, 5), 1.0);
        let out = lumenply_render::composite_raster(&doc);
        assert!(out.get(30, 30).a < 0.01 && out.get(5, 5).a > 0.99);
        SetMaskEnabled {
            layer: id,
            enabled: false,
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            lumenply_render::composite_raster(&doc).get(30, 30).a > 0.99,
            "disabled mask is ignored"
        );
        RemoveMask { layer: id }.apply(&mut doc).unwrap();
        assert!(doc.layer(id).unwrap().mask.is_none());
        assert!(PaintMask {
            layer: id,
            brush: black,
            points: vec![StrokePoint::new(1.0, 1.0, 1.0)]
        }
        .apply(&mut doc)
        .is_err());
    }

    #[test]
    fn group_and_ungroup_keep_order() {
        let mut doc = Document::new(10, 10);
        let a = doc.add_pixel_layer("a");
        let b = doc.add_pixel_layer("b");
        let c = doc.add_pixel_layer("c");
        let _d = doc.add_pixel_layer("d");
        GroupLayers {
            layers: vec![c, b],
            name: "G".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let top: Vec<&str> = doc.layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(top, ["a", "G", "d"]);
        let g = doc.layers()[1].id;
        let inner: Vec<LayerId> = doc
            .layer(g)
            .unwrap()
            .children()
            .unwrap()
            .iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(inner, vec![b, c], "children keep bottom-to-top order");
        assert_eq!(doc.parent_of(b), Some(g));
        assert_eq!(doc.parent_of(a), None);
        // Reorder inside the group works through siblings.
        ReorderLayer { layer: b, delta: 1 }.apply(&mut doc).unwrap();
        let inner: Vec<LayerId> = doc
            .layer(g)
            .unwrap()
            .children()
            .unwrap()
            .iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(inner, vec![c, b]);
        assert!(
            GroupLayers {
                layers: vec![a, b],
                name: "X".into()
            }
            .apply(&mut doc)
            .is_err(),
            "not siblings"
        );
        UngroupLayer { layer: g }.apply(&mut doc).unwrap();
        let top: Vec<&str> = doc.layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(top, ["a", "c", "b", "d"]);
        assert!(UngroupLayer { layer: a }.apply(&mut doc).is_err());
    }

    #[test]
    fn crop_and_canvas_and_image_size() {
        let mut doc = Document::new(100, 80);
        let id = doc.add_pixel_layer("p");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(40, 30, Rgba::WHITE);

        CropDocument {
            rect: Rect::new(30, 20, 50, 40),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height), (50, 40));
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(10, 10),
            Rgba::WHITE
        );

        ResizeCanvas {
            width: 70,
            height: 60,
            anchor: (0.5, 0.5),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(20, 20),
            Rgba::WHITE
        );

        ResizeImage {
            width: 140,
            height: 120,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height), (140, 120));
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let total: f32 = (0..140)
            .flat_map(|y| (0..140).map(move |x| (x, y)))
            .map(|(x, y)| px.get_pixel(x, y).a)
            .sum();
        assert!(
            (total - 4.0).abs() < 0.3,
            "doubling spreads one pixel over ~4: {total}"
        );
        assert!(ResizeImage { width: 0, height: 5 }.apply(&mut doc).is_err());
    }

    #[test]
    fn filter_respects_selection() {
        let mut doc = Document::new(80, 40);
        let id = doc.add_pixel_layer("p");
        for x in 0..80 {
            let v = if x < 40 { 0.2 } else { 0.8 };
            for y in 0..40 {
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 80, 20)));
        ApplyFilter {
            layer: id,
            filter: Filter::GaussianBlur { radius: 6.0 },
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let blurred = px.get_pixel(40, 10).r;
        let crisp = px.get_pixel(40, 30).r;
        assert!(
            blurred > 0.3 && blurred < 0.7,
            "inside selection is blurred: {blurred}"
        );
        assert!((crisp - 0.8).abs() < 1e-3, "outside selection untouched: {crisp}");
    }

    #[test]
    fn filter_layer_commands() {
        let mut doc = Document::new(8, 8);
        AddFilterLayer::new(Filter::BoxBlur { radius: 2.0 })
            .apply(&mut doc)
            .unwrap();
        let id = doc.layers()[0].id;
        assert!(matches!(
            doc.layer(id).unwrap().content,
            LayerContent::Filter(Filter::BoxBlur { .. })
        ));
        SetFilter {
            layer: id,
            filter: Filter::GaussianBlur { radius: 9.0 },
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            matches!(doc.layer(id).unwrap().content, LayerContent::Filter(Filter::GaussianBlur { radius }) if radius == 9.0)
        );
        let px = doc.add_pixel_layer("p");
        assert!(SetFilter {
            layer: px,
            filter: Filter::BoxBlur { radius: 1.0 }
        }
        .apply(&mut doc)
        .is_err());
    }

    #[test]
    fn bucket_wand_and_gradient() {
        let mut doc = Document::new(60, 40);
        let id = doc.add_pixel_layer("p");
        // Two regions: left half grey, right half white, with a thin wall of
        // the same grey separating a pocket on the right.
        let px = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..40 {
            for x in 0..60 {
                let v = if x < 30 || x == 45 { 0.5 } else { 1.0 };
                px.set_pixel(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        // Contiguous fill of the right-of-wall pocket only.
        BucketFill {
            layer: id,
            x: 50,
            y: 20,
            color: [1.0, 0.0, 0.0, 1.0],
            tolerance: 0.1,
            contiguous: true,
            sample: SampleSource::Layer(id),
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        assert!(px.get_pixel(50, 20).to_straight()[0] > 0.99 && px.get_pixel(50, 20).to_straight()[1] < 0.01);
        assert!(
            (px.get_pixel(35, 20).to_straight()[1] - 1.0).abs() < 1e-4,
            "other white region untouched"
        );
        assert!(
            (px.get_pixel(10, 20).to_straight()[0] - 0.5).abs() < 1e-4,
            "grey untouched"
        );

        // Non-contiguous wand on grey picks both grey areas (left half + wall).
        MagicWandSelect {
            x: 5,
            y: 5,
            tolerance: 0.05,
            contiguous: false,
            sample: SampleSource::Layer(id),
            op: CombineOp::Replace,
        }
        .apply(&mut doc)
        .unwrap();
        let sel = doc.selection.as_ref().unwrap();
        assert_eq!(sel.value(10, 10), 1.0);
        assert_eq!(sel.value(45, 10), 1.0, "the wall is the same grey");
        assert_eq!(sel.value(35, 10), 0.0);

        // Gradient within that selection, left (blue) to right (transparent).
        GradientFill {
            layer: id,
            start: (0.0, 0.0),
            end: (30.0, 0.0),
            colors: [[0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 0.0]],
            kind: GradientKind::Linear,
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let left = px.get_pixel(0, 20).to_straight();
        let mid = px.get_pixel(15, 20).to_straight();
        assert!(left[2] > 0.95, "fully blue at the start: {left:?}");
        assert!(mid[2] > 0.6 && mid[2] < 0.9, "mixed halfway: {mid:?}");
        assert!(
            (px.get_pixel(35, 20).to_straight()[1] - 1.0).abs() < 1e-4,
            "outside the selection untouched"
        );

        // Merged sampling sees the composite.
        assert!(
            BucketFill {
                layer: id,
                x: 100,
                y: 100,
                color: [0.0; 4],
                tolerance: 0.0,
                contiguous: true,
                sample: SampleSource::Merged
            }
            .apply(&mut doc)
            .is_ok(),
            "a seed outside the canvas is a no-op"
        );
    }

    #[test]
    fn work_path_fills_strokes_and_selects() {
        use lumenply_doc::{PathNode, SubPath, VectorPath};
        let mut doc = Document::new(100, 100);
        let id = doc.add_pixel_layer("L");

        // A near-circle of radius 30 around (50,50): four smooth nodes with
        // the classic kappa handles.
        let k = 30.0 * 0.5523;
        let node = |p: (f32, f32), hin: (f32, f32), hout: (f32, f32)| PathNode {
            point: p,
            handle_in: hin,
            handle_out: hout,
        };
        let circle = VectorPath {
            subpaths: vec![SubPath {
                closed: true,
                nodes: vec![
                    node((80.0, 50.0), (80.0, 50.0 - k), (80.0, 50.0 + k)),
                    node((50.0, 80.0), (50.0 + k, 80.0), (50.0 - k, 80.0)),
                    node((20.0, 50.0), (20.0, 50.0 + k), (20.0, 50.0 - k)),
                    node((50.0, 20.0), (50.0 - k, 20.0), (50.0 + k, 20.0)),
                ],
            }],
        };
        SetWorkPath {
            path: Some(circle.clone()),
        }
        .apply(&mut doc)
        .unwrap();

        // Flattening hits the on-curve points and the circle's extremes.
        let flat = doc.work_path.as_ref().unwrap().flatten();
        assert_eq!(flat.len(), 1);
        let (pts, closed) = &flat[0];
        assert!(*closed);
        let on_circle = pts
            .iter()
            .all(|&(x, y)| (((x - 50.0).powi(2) + (y - 50.0).powi(2)).sqrt() - 30.0).abs() < 0.6);
        assert!(on_circle, "flattened points stay near the circle");

        // Fill: area within a couple of percent of a 30 px circle.
        FillPath {
            layer: id,
            color: [0.9, 0.2, 0.1, 1.0],
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let mut area = 0.0;
        for y in 0..100 {
            for x in 0..100 {
                area += px.get_pixel(x, y).a;
            }
        }
        let expect = std::f32::consts::PI * 30.0 * 30.0;
        assert!(
            (area - expect).abs() / expect < 0.02,
            "fill area {area} vs {expect}"
        );
        assert!(px.get_pixel(50, 50).a > 0.99, "centre filled");
        assert!(px.get_pixel(5, 5).a < 1e-5, "corner empty");

        // Selection from path matches the fill's coverage.
        PathToSelection {
            op: CombineOp::Replace,
        }
        .apply(&mut doc)
        .unwrap();
        let sel = doc.selection.as_ref().unwrap();
        assert!((sel.value(50, 50) - 1.0).abs() < 1e-4);
        assert!(sel.value(5, 5) <= 1e-5);

        // Stroke runs paint along the outline: on-path painted, centre not.
        let mut doc2 = Document::new(100, 100);
        let id2 = doc2.add_pixel_layer("L");
        doc2.work_path = Some(circle);
        StrokeWorkPath {
            layer: id2,
            brush: Brush {
                radius: 3.0,
                hardness: 1.0,
                color: [0.0, 0.0, 1.0, 1.0],
                ..Brush::default()
            },
        }
        .apply(&mut doc2)
        .unwrap();
        let px = doc2.layer(id2).unwrap().pixels().unwrap();
        assert!(px.get_pixel(80, 50).a > 0.9, "outline painted");
        assert!(px.get_pixel(50, 50).a < 1e-5, "centre untouched");

        // No path → a clear error.
        assert!(FillPath {
            layer: id2,
            color: [0.0; 4]
        }
        .apply(&mut Document::new(8, 8))
        .is_err());
    }

    #[test]
    fn perspective_layer_keystones_pixels_and_mask_together() {
        let mut doc = Document::new(64, 64);
        let id = doc.add_pixel_layer("L");
        for y in 10..50 {
            for x in 10..50 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.0, 0.6, 0.9, 1.0),
                );
            }
        }
        // Mask hides the left half of the square.
        let mut mask = Mask::reveal_all();
        for y in 10..50 {
            for x in 10..30 {
                mask.set_value(x, y, 0.0);
            }
        }
        doc.layer_mut(id).unwrap().mask = Some(mask);

        // Narrow the top edge to its middle half.
        let cmd = PerspectiveLayer {
            layer: id,
            quad: [(20.0, 10.0), (40.0, 10.0), (50.0, 50.0), (10.0, 50.0)],
        };
        let aff = cmd.affected(&doc).unwrap();
        cmd.apply(&mut doc).unwrap();
        let l = doc.layer(id).unwrap();
        let px = l.pixels().unwrap();
        assert!(px.get_pixel(30, 12).a > 0.9, "top centre kept");
        assert!(px.get_pixel(14, 12).a < 0.05, "top corner cut by the keystone");
        assert!(px.get_pixel(12, 47).a > 0.9, "bottom keeps its width");
        assert!(
            aff.contains(30, 12) && aff.contains(12, 47),
            "affected covers the warp"
        );
        // The mask edge (x = 30 in the source, the square's midline) stays
        // on the warped midline: hidden left of it, shown right of it.
        let m = l.mask.as_ref().unwrap();
        assert!(m.value(25, 30) < 0.1, "left stays hidden after the warp");
        assert!(m.value(38, 30) > 0.9, "right stays revealed");

        // Text layers refuse with a clear message; degenerate quads error.
        let tid = doc.alloc_id();
        doc.add_layer(Layer::text(
            tid,
            lumenply_doc::TextLayer::new("x", 0.0, 10.0, 12.0, [0.0, 0.0, 0.0, 1.0]),
        ));
        assert!(PerspectiveLayer {
            layer: tid,
            quad: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        }
        .apply(&mut doc)
        .is_err());
        assert!(PerspectiveLayer {
            layer: id,
            quad: [(0.0, 0.0); 4]
        }
        .apply(&mut doc)
        .is_err());
    }

    #[test]
    fn warp_layer_bends_pixels_and_mask_together() {
        let mut doc = Document::new(64, 64);
        let id = doc.add_pixel_layer("L");
        for y in 8..56 {
            for x in 8..56 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.3, 0.6, 0.9, 1.0),
                );
            }
        }
        // Mask hides the left half.
        let mut mask = Mask::reveal_all();
        for y in 8..56 {
            for x in 8..32 {
                mask.set_value(x, y, 0.0);
            }
        }
        doc.layer_mut(id).unwrap().mask = Some(mask);
        let src = doc.layer(id).unwrap().pixels().unwrap().content_bounds().unwrap();

        // Pull the centre of a 2×2-cell mesh 10 px right: the mask's
        // vertical edge at mid-height moves with it, the top edge stays.
        let mut grid = lumenply_render::WarpGrid::identity(src, 2, 2);
        grid.points[4].0 += 10.0;
        let cmd = WarpLayer { layer: id, grid };
        assert!(cmd.affected(&doc).unwrap().contains(40, 32));
        cmd.apply(&mut doc).unwrap();
        let m = doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(m.value(38, 32) < 0.1, "mask edge bent right at mid-height");
        assert!(m.value(34, 9) > 0.9, "mask edge stays put at the pinned top");
        assert!(doc.layer(id).unwrap().pixels().unwrap().get_pixel(32, 32).a > 0.99);

        // A malformed mesh and a text layer both refuse.
        let mut bad = lumenply_render::WarpGrid::identity(src, 2, 2);
        bad.points.truncate(3);
        assert!(WarpLayer { layer: id, grid: bad }.apply(&mut doc).is_err());
        let tid = doc.alloc_id();
        doc.add_layer(Layer::text(
            tid,
            lumenply_doc::TextLayer::new("x", 0.0, 10.0, 12.0, [0.0, 0.0, 0.0, 1.0]),
        ));
        assert!(WarpLayer {
            layer: tid,
            grid: lumenply_render::WarpGrid::identity(src, 2, 2)
        }
        .apply(&mut doc)
        .is_err());
    }

    #[test]
    fn smart_objects_transform_without_degrading() {
        // A fine checkerboard: the sharpest content there is, so repeated
        // resampling degrades it measurably unless re-rendered from source.
        let mut doc = Document::new(128, 128);
        let id = doc.add_pixel_layer("L");
        for y in 32..96 {
            for x in 32..96 {
                let v = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(v, v, v, 1.0),
                );
            }
        }
        let reference = doc.clone();
        ConvertToSmartObject { layer: id }.apply(&mut doc).unwrap();
        assert!(doc.layer(id).unwrap().smart_layer().is_some());

        // Shrink to a quarter, then scale back up 4x: a destructive
        // round trip would blur the checker to grey; the smart object
        // re-renders from the source, identical to one direct resample.
        let around = |k: f32| TransformLayer {
            layer: id,
            transform: Affine::around(64.0, 64.0, k, k, 0.0),
        };
        around(0.25).apply(&mut doc).unwrap();
        around(4.0).apply(&mut doc).unwrap();
        let s = doc.layer(id).unwrap().smart_layer().unwrap();
        assert!(
            (s.transform.a - 1.0).abs() < 1e-5 && s.transform.tx.abs() < 1e-3,
            "transforms composed back to identity: {:?}",
            s.transform
        );
        let smart = doc.layer(id).unwrap().raster_store().unwrap();
        let mut max_err = 0.0f32;
        for y in 40..88 {
            for x in 40..88 {
                let a = smart.get_pixel(x, y).to_straight()[0];
                let b = reference
                    .layer(id)
                    .unwrap()
                    .pixels()
                    .unwrap()
                    .get_pixel(x, y)
                    .to_straight()[0];
                max_err = max_err.max((a - b).abs());
            }
        }
        assert!(max_err < 0.02, "checker survives down-then-up: max err {max_err}");

        // The destructive path, for contrast, loses the checker entirely.
        let mut destructive = reference.clone();
        TransformLayer {
            layer: id,
            transform: Affine::around(64.0, 64.0, 0.25, 0.25, 0.0),
        }
        .apply(&mut destructive)
        .unwrap();
        TransformLayer {
            layer: id,
            transform: Affine::around(64.0, 64.0, 4.0, 4.0, 0.0),
        }
        .apply(&mut destructive)
        .unwrap();
        let p = destructive.layer(id).unwrap().pixels().unwrap();
        let mid = p.get_pixel(64, 64).to_straight()[0];
        assert!(
            (0.1..0.9).contains(&mid),
            "destructive round trip greys the checker (sanity): {mid}"
        );

        // Rasterize turns the rendered result back into plain pixels.
        RasterizeLayer { layer: id }.apply(&mut doc).unwrap();
        assert!(doc.layer(id).unwrap().pixels().is_some());

        // Converting anything but a pixel layer refuses.
        let gid = doc.add_group("g");
        assert!(ConvertToSmartObject { layer: gid }.apply(&mut doc).is_err());
    }

    #[test]
    fn float_mode_keeps_hdr_values_until_switched_off() {
        use crate::Editor;
        let mut doc = Document::new(16, 16);
        let id = doc.add_pixel_layer("L");
        doc.float_mode = true;
        doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
            2,
            2,
            Rgba::from_straight(3.5, 0.5, 0.25, 1.0),
        );
        let mut ed = Editor::new(doc);

        // An unrelated edit would normally compact (and clamp) the tile;
        // float mode must leave the 3.5 alone.
        ed.execute(&PaintStroke {
            layer: id,
            brush: Brush {
                radius: 2.0,
                color: [0.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: vec![StrokePoint::new(12.0, 12.0, 1.0)],
        })
        .unwrap();
        let p = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(2, 2);
        assert!((p.to_straight()[0] - 3.5).abs() < 1e-5, "HDR red survives: {p:?}");

        // Switching float mode off clamps lazily: the shared tile keeps
        // its value until an edit touches it, then compaction clamps.
        ed.execute(&SetFloatMode { on: false }).unwrap();
        assert!(!ed.doc().float_mode);
        ed.execute(&PaintStroke {
            layer: id,
            brush: Brush {
                radius: 1.0,
                color: [0.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: vec![StrokePoint::new(6.0, 2.0, 1.0)], // same tile, away from (2,2)
        })
        .unwrap();
        let p = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(2, 2);
        assert!(
            p.to_straight()[0] <= 1.0 + 1e-4,
            "clamped once the tile is edited: {p:?}"
        );

        // Undo restores both the flag and the HDR value (snapshots were
        // never compacted while the flag was on).
        ed.undo().unwrap();
        ed.undo().unwrap();
        assert!(ed.doc().float_mode);
        let p = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(2, 2);
        assert!(
            (p.to_straight()[0] - 3.5).abs() < 1e-5,
            "undo brings the HDR value back: {p:?}"
        );
    }

    #[test]
    fn saved_paths_store_load_and_delete() {
        use lumenply_doc::{PathNode, SubPath, VectorPath};
        let tri = VectorPath {
            subpaths: vec![SubPath {
                closed: true,
                nodes: vec![
                    PathNode::corner(10.0, 10.0),
                    PathNode::corner(50.0, 10.0),
                    PathNode::corner(30.0, 40.0),
                ],
            }],
        };
        let mut doc = Document::new(64, 64);
        // Saving without a work path is a clear error.
        assert!(SaveWorkPath { name: "a".into() }.apply(&mut doc).is_err());
        doc.work_path = Some(tri.clone());
        SaveWorkPath {
            name: "Outline".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.saved_paths.len(), 1);
        assert_eq!(doc.saved_paths[0].name, "Outline");

        // Clearing the work path leaves the saved copy intact; loading
        // brings it back.
        SetWorkPath { path: None }.apply(&mut doc).unwrap();
        assert!(doc.work_path.is_none());
        UseSavedPath { index: 0 }.apply(&mut doc).unwrap();
        assert_eq!(doc.work_path.as_ref(), Some(&tri));
        assert!(UseSavedPath { index: 3 }.apply(&mut doc).is_err());

        DeleteSavedPath { index: 0 }.apply(&mut doc).unwrap();
        assert!(doc.saved_paths.is_empty());
        assert!(DeleteSavedPath { index: 0 }.apply(&mut doc).is_err());
        // The work path copy survives the delete.
        assert_eq!(doc.work_path.as_ref(), Some(&tri));
    }

    #[test]
    fn subpath_inside_another_cuts_a_hole() {
        use lumenply_doc::{PathNode, SubPath, VectorPath};
        // A donut: outer square 10..90, inner square 30..70, both as
        // corner-node subpaths of one path. Even-odd fill leaves the ring.
        let square = |a: f32, b: f32| SubPath {
            closed: true,
            nodes: vec![
                PathNode::corner(a, a),
                PathNode::corner(b, a),
                PathNode::corner(b, b),
                PathNode::corner(a, b),
            ],
        };
        let mut doc = Document::new(100, 100);
        let id = doc.add_pixel_layer("L");
        doc.work_path = Some(VectorPath {
            subpaths: vec![square(10.0, 90.0), square(30.0, 70.0)],
        });
        FillPath {
            layer: id,
            color: [0.0, 0.5, 1.0, 1.0],
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        assert!(px.get_pixel(20, 50).a > 0.99, "ring filled");
        assert!(px.get_pixel(50, 50).a < 1e-5, "hole stays empty");
        assert!(px.get_pixel(5, 50).a < 1e-5, "outside stays empty");
        // Selection from the same path shows the same hole.
        PathToSelection {
            op: CombineOp::Replace,
        }
        .apply(&mut doc)
        .unwrap();
        let sel = doc.selection.as_ref().unwrap();
        assert!(sel.value(20, 50) > 0.99 && sel.value(50, 50) < 1e-5);
    }

    #[test]
    fn spot_heal_melts_blemishes_into_flat_and_gradient_surroundings() {
        // Flat green with a red blob.
        let mut doc = Document::new(64, 64);
        let id = doc.add_pixel_layer("L");
        for y in 0..64 {
            for x in 0..64 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.7, 0.2, 1.0),
                );
            }
        }
        for y in 28..37 {
            for x in 28..37 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.9, 0.1, 0.1, 1.0),
                );
            }
        }
        HealStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                hardness: 0.7,
                color: [0.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: vec![StrokePoint::new(32.0, 32.0, 1.0)],
            offset: (0, 0),
            sample: SampleSource::Layer(id),
            texture: false,
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let c = px.get_pixel(32, 32).to_straight();
        assert!(
            (c[0] - 0.2).abs() < 0.05 && (c[1] - 0.7).abs() < 0.05,
            "blob healed to the surround: {c:?}"
        );
        assert!((c[3] - 1.0).abs() < 1e-5, "alpha untouched");
        let far = px.get_pixel(5, 5).to_straight();
        assert!((far[1] - 0.7).abs() < 1e-5, "outside untouched");

        // A horizontal ramp with a blob: diffusion follows the gradient,
        // which a plain clone or flat fill would not.
        let mut doc = Document::new(64, 64);
        let id = doc.add_pixel_layer("L");
        for y in 0..64 {
            for x in 0..64 {
                let v = x as f32 / 63.0;
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(v, v, v, 1.0),
                );
            }
        }
        for y in 28..37 {
            for x in 28..37 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        HealStroke {
            layer: id,
            brush: Brush {
                radius: 10.0,
                hardness: 0.7,
                color: [0.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: vec![StrokePoint::new(32.0, 32.0, 1.0)],
            offset: (0, 0),
            sample: SampleSource::Layer(id),
            texture: false,
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let c = px.get_pixel(32, 32).to_straight();
        let local = 32.0 / 63.0;
        assert!(
            (c[0] - local).abs() < 0.12 && (c[0] - c[1]).abs() < 0.03,
            "healed onto the ramp: {c:?} vs {local}"
        );
    }

    #[test]
    fn healing_with_texture_keeps_detail_at_the_destination_tone() {
        // Destination: flat dark grey with a bright blemish. Source region
        // (offset away): bright vertical stripes.
        let mut doc = Document::new(96, 48);
        let id = doc.add_pixel_layer("L");
        for y in 0..48 {
            for x in 0..96 {
                let v = if x >= 64 {
                    if (x / 4) % 2 == 0 {
                        0.9
                    } else {
                        0.5
                    }
                } else {
                    0.2
                };
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(v, v, v, 1.0),
                );
            }
        }
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(24, 24, Rgba::WHITE);
        HealStroke {
            layer: id,
            brush: Brush {
                radius: 9.0,
                hardness: 0.8,
                color: [0.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: vec![StrokePoint::new(24.0, 24.0, 1.0)],
            offset: (50, 0), // sample from the striped region
            sample: SampleSource::Layer(id),
            texture: true,
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        let mut mean = 0.0;
        let mut n = 0.0;
        for x in 19..30 {
            let v = px.get_pixel(x, 24).to_straight()[0];
            lo = lo.min(v);
            hi = hi.max(v);
            mean += v;
            n += 1.0;
        }
        mean /= n;
        assert!((mean - 0.2).abs() < 0.1, "tone follows the destination: {mean}");
        assert!(hi - lo > 0.08, "stripes carried over as texture: {lo}..{hi}");
    }

    #[test]
    fn painting_the_selection_selects_and_deselects() {
        let mut doc = Document::new(64, 64);
        let white = Brush {
            radius: 6.0,
            hardness: 1.0,
            color: [1.0, 1.0, 1.0, 1.0],
            ..Brush::default()
        };
        PaintSelection {
            brush: white,
            points: vec![StrokePoint::new(32.0, 32.0, 1.0)],
        }
        .apply(&mut doc)
        .unwrap();
        let sel = doc.selection.as_ref().unwrap();
        assert!((sel.value(32, 32) - 1.0).abs() < 1e-4, "painted selected");
        assert!(sel.value(5, 5) <= 1e-5, "elsewhere empty");

        // Black paint deselects, even though a selection exists (strokes
        // must not be clipped by the selection they edit).
        let black = Brush {
            radius: 3.0,
            hardness: 1.0,
            color: [0.0, 0.0, 0.0, 1.0],
            ..Brush::default()
        };
        PaintSelection {
            brush: black,
            points: vec![StrokePoint::new(32.0, 32.0, 1.0)],
        }
        .apply(&mut doc)
        .unwrap();
        let sel = doc.selection.as_ref().unwrap();
        assert!(sel.value(32, 32) <= 1e-4, "black deselects the centre");
        assert!(sel.value(36, 32) > 0.5, "ring stays selected");

        // Erasing everything clears the selection entirely.
        let eraser = Brush {
            radius: 40.0,
            hardness: 1.0,
            color: [0.0, 0.0, 0.0, 1.0],
            mode: BrushMode::Erase,
            ..Brush::default()
        };
        PaintSelection {
            brush: eraser,
            points: vec![StrokePoint::new(32.0, 32.0, 1.0)],
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.selection.is_none(), "fully erased selection clears");
    }

    #[test]
    fn color_range_selects_matching_pixels_with_soft_edges() {
        let mut doc = Document::new(64, 32);
        let id = doc.add_pixel_layer("L");
        for y in 0..32 {
            for x in 0..64 {
                let c = if x < 32 {
                    Rgba::from_straight(0.8, 0.1, 0.1, 1.0) // red
                } else {
                    Rgba::from_straight(0.1, 0.1, 0.8, 1.0) // blue
                };
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, c);
            }
        }
        SelectColorRange {
            color: [0.8, 0.1, 0.1],
            tolerance: 0.3,
        }
        .apply(&mut doc)
        .unwrap();
        let sel = doc.selection.as_ref().unwrap();
        assert!((sel.value(10, 10) - 1.0).abs() < 1e-5, "red fully selected");
        assert!(sel.value(50, 10) <= 0.0 + 1e-5, "blue not selected");

        // A colour at the tolerance boundary gets graded coverage.
        doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
            0,
            0,
            Rgba::from_straight(0.8 - 0.22, 0.1, 0.1, 1.0),
        );
        SelectColorRange {
            color: [0.8, 0.1, 0.1],
            tolerance: 0.3,
        }
        .apply(&mut doc)
        .unwrap();
        let v = doc.selection.as_ref().unwrap().value(0, 0);
        assert!(v > 0.2 && v < 0.8, "soft edge: {v}");

        // No match clears the selection rather than leaving an empty one.
        SelectColorRange {
            color: [0.0, 1.0, 0.0],
            tolerance: 0.05,
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.selection.is_none());
    }

    #[test]
    fn dodge_lightens_burn_darkens_and_jitter_is_deterministic() {
        let mut doc = Document::new(64, 64);
        let id = doc.add_pixel_layer("L");
        let grey = Rgba::from_straight(0.5, 0.5, 0.5, 1.0);
        for y in 0..64 {
            for x in 0..64 {
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, grey);
            }
        }
        let stroke = |mode, strength: f32| PaintStroke {
            layer: id,
            brush: Brush {
                radius: 6.0,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, strength],
                mode,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(32.0, 32.0, 1.0)],
        };
        let mut d = doc.clone();
        stroke(BrushMode::Dodge, 0.5).apply(&mut d).unwrap();
        let p = d
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(32, 32)
            .to_straight();
        let expect = lumenply_doc::adjust::srgb_decode(
            lumenply_doc::adjust::srgb_encode(0.5) + (1.0 - lumenply_doc::adjust::srgb_encode(0.5)) * 0.5,
        );
        assert!((p[0] - expect).abs() < 1e-3, "dodge: {} vs {expect}", p[0]);
        assert!((p[3] - 1.0).abs() < 1e-5, "alpha untouched");
        let edge = d
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(50, 50)
            .to_straight();
        assert!((edge[0] - 0.5).abs() < 1e-5, "outside the dab untouched");

        let mut b = doc.clone();
        stroke(BrushMode::Burn, 0.5).apply(&mut b).unwrap();
        let p = b
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(32, 32)
            .to_straight();
        let expect = lumenply_doc::adjust::srgb_decode(lumenply_doc::adjust::srgb_encode(0.5) * 0.5);
        assert!((p[0] - expect).abs() < 1e-3, "burn: {} vs {expect}", p[0]);

        // Jitter scatters but replays identically and stays inside the
        // padded bounds.
        let jittered = PaintStroke {
            layer: id,
            brush: Brush {
                radius: 4.0,
                jitter: 1.0,
                color: [1.0, 0.0, 0.0, 1.0],
                ..Brush::default()
            },
            points: (0..20)
                .map(|i| StrokePoint::new(10.0 + i as f32, 32.0, 1.0))
                .collect(),
        };
        let bounds = stroke_bounds(&jittered.brush, &jittered.points, doc.canvas());
        let (mut a, mut c) = (doc.clone(), doc.clone());
        jittered.apply(&mut a).unwrap();
        jittered.apply(&mut c).unwrap();
        let (pa, pc) = (
            a.layer(id).unwrap().pixels().unwrap(),
            c.layer(id).unwrap().pixels().unwrap(),
        );
        let mut scattered = false;
        for y in 0..64 {
            for x in 0..64 {
                assert_eq!(pa.get_pixel(x, y), pc.get_pixel(x, y), "deterministic at {x},{y}");
                if pa.get_pixel(x, y).to_straight()[0] > 0.6 {
                    assert!(bounds.contains(x, y), "stays inside stroke_bounds");
                    if (y - 32).abs() > 2 {
                        scattered = true;
                    }
                }
            }
        }
        assert!(scattered, "jitter actually scattered dabs");
    }

    #[test]
    fn smudge_drags_colour_and_sponge_scales_saturation() {
        // Left half red, right half white; a rightward smudge across the
        // boundary drags red into the white side.
        let mut doc = Document::new(64, 32);
        let id = doc.add_pixel_layer("L");
        for y in 0..32 {
            for x in 0..64 {
                let c = if x < 32 {
                    Rgba::from_straight(1.0, 0.0, 0.0, 1.0)
                } else {
                    Rgba::from_straight(1.0, 1.0, 1.0, 1.0)
                };
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, c);
            }
        }
        let mut d = doc.clone();
        PaintStroke {
            layer: id,
            brush: Brush {
                radius: 5.0,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, 0.9],
                spacing: 0.4,
                mode: BrushMode::Smudge,
                ..Brush::default()
            },
            points: (0..=24)
                .map(|i| StrokePoint::new(20.0 + i as f32, 16.0, 1.0))
                .collect(),
        }
        .apply(&mut d)
        .unwrap();
        let px = d.layer(id).unwrap().pixels().unwrap();
        let dragged = px.get_pixel(36, 16).to_straight();
        assert!(
            dragged[1] < 0.6,
            "red dragged past the boundary (green sank): {dragged:?}"
        );
        let untouched = px.get_pixel(36, 2).to_straight();
        assert!(untouched[1] > 0.99, "off the stroke stays white: {untouched:?}");
        let alpha_kept = px.get_pixel(30, 16);
        assert!((alpha_kept.a - 1.0).abs() < 1e-4, "opaque stays opaque");

        // Sponge: desaturate pulls a saturated red toward grey; saturate
        // pushes a muted tone away from it. Both leave alpha alone.
        let enc = lumenply_doc::adjust::srgb_encode;
        let dec = lumenply_doc::adjust::srgb_decode;
        let mut doc = Document::new(16, 16);
        let id = doc.add_pixel_layer("L");
        let muted = Rgba::from_straight(dec(0.6), dec(0.4), dec(0.4), 1.0);
        for y in 0..16 {
            for x in 0..16 {
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, muted);
            }
        }
        let sponge = |mode, doc: &Document| {
            let mut d = doc.clone();
            PaintStroke {
                layer: id,
                brush: Brush {
                    radius: 4.0,
                    hardness: 1.0,
                    color: [0.0, 0.0, 0.0, 1.0],
                    mode,
                    ..Brush::default()
                },
                points: vec![StrokePoint::new(8.0, 8.0, 1.0)],
            }
            .apply(&mut d)
            .unwrap();
            d.layer(id)
                .unwrap()
                .pixels()
                .unwrap()
                .get_pixel(8, 8)
                .to_straight()
        };
        let y = 0.2126 * 0.6 + 0.7152 * 0.4 + 0.0722 * 0.4;
        let de = sponge(BrushMode::Desaturate, &doc);
        assert!(
            (enc(de[0]) - y).abs() < 2e-3 && (enc(de[1]) - y).abs() < 2e-3,
            "full-strength desaturate reaches grey y={y}: {de:?}"
        );
        let sa = sponge(BrushMode::Saturate, &doc);
        assert!(
            (enc(sa[0]) - (y + (0.6 - y) * 2.0)).abs() < 2e-3,
            "saturate doubles the chroma: {sa:?}"
        );
        assert!((sa[3] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn smoothing_passes_through_input_points_and_bends_between_them() {
        let pts = vec![
            StrokePoint::new(0.0, 0.0, 1.0),
            StrokePoint::new(50.0, 0.0, 1.0),
            StrokePoint::new(50.0, 50.0, 1.0),
        ];
        let sm = smooth_stroke(&pts);
        assert!(sm.len() > pts.len());
        let first = sm.first().unwrap();
        let last = sm.last().unwrap();
        assert!((first.x, first.y) == (0.0, 0.0) && (last.x, last.y) == (50.0, 50.0));
        // Around the corner the curve cuts inside the polyline's corner point.
        let near_corner = sm
            .iter()
            .filter(|p| (p.x - 50.0).abs() < 1e-3 && (p.y - 0.0).abs() < 1e-3)
            .count();
        assert_eq!(near_corner, 1, "the corner itself is visited once");
        // The curve leaves both straight segments near the corner (it is curved, not a polyline).
        let off_segments = sm
            .iter()
            .filter(|p| p.y.abs() > 0.5 && (p.x - 50.0).abs() > 0.5)
            .count();
        assert!(off_segments >= 2, "curved points near the corner: {off_segments}");
        assert_eq!(smooth_stroke(&pts[..2]).len(), 2, "two points stay a line");
    }

    #[test]
    fn text_layers_add_edit_move_and_rasterize() {
        let mut doc = Document::new(400, 200);
        AddTextLayer {
            text: TextLayer::new("Hi", 20.0, 100.0, 48.0, [0.0, 0.0, 0.0, 1.0]),
            above: None,
        }
        .apply(&mut doc)
        .unwrap();
        let id = doc.layers()[0].id;
        assert_eq!(doc.layer(id).unwrap().name, "Hi");
        let b1 = doc
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .content_bounds()
            .unwrap();
        assert!(b1.x >= 18 && b1.x < 30);

        let mut t = doc.layer(id).unwrap().text_layer().unwrap().clone();
        t.text = "Hello world".into();
        SetText { layer: id, text: t }.apply(&mut doc).unwrap();
        assert_eq!(doc.layer(id).unwrap().name, "Hello world");
        let b2 = doc
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .content_bounds()
            .unwrap();
        assert!(b2.w > b1.w * 2);

        MoveLayer {
            layer: id,
            dx: 100,
            dy: 0,
        }
        .apply(&mut doc)
        .unwrap();
        let b3 = doc
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .content_bounds()
            .unwrap();
        assert_eq!(b3.x, b2.x + 100);
        assert!(
            TransformLayer::around_center(&doc, id, 2.0, 2.0, 0.0).is_none(),
            "no pixels() on text"
        );
        assert!(TransformLayer {
            layer: id,
            transform: Affine::scale(2.0, 2.0)
        }
        .apply(&mut doc)
        .is_err());

        RasterizeLayer { layer: id }.apply(&mut doc).unwrap();
        assert!(doc.layer(id).unwrap().pixels().is_some());
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().content_bounds().unwrap(),
            b3
        );
        assert!(RasterizeLayer { layer: id }.apply(&mut doc).is_err());
    }

    #[test]
    fn clone_stamp_copies_from_the_offset() {
        let mut doc = Document::new(100, 50);
        let id = doc.add_pixel_layer("p");
        for y in 0..50 {
            for x in 0..50 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        CloneStroke {
            layer: id,
            brush: Brush {
                radius: 6.0,
                hardness: 1.0,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(75.0, 25.0, 1.0)],
            offset: (-50, 0),
            sample: SampleSource::Layer(id),
        }
        .apply(&mut doc)
        .unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let p = px.get_pixel(75, 25).to_straight();
        assert!(
            p[0] > 0.99 && p[3] > 0.99,
            "red cloned into the empty half: {p:?}"
        );
        assert!(px.get_pixel(90, 25).is_transparent());
    }

    #[test]
    fn rotate_and_flip_the_whole_image() {
        let mut doc = Document::new(40, 20);
        let id = doc.add_pixel_layer("p");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(0, 0, Rgba::WHITE); // top-left
        RotateImage { quarter_turns: 1 }.apply(&mut doc).unwrap();
        assert_eq!((doc.width, doc.height), (20, 40));
        let px = doc.layer(id).unwrap().pixels().unwrap();
        assert_eq!(
            px.get_pixel(19, 0),
            Rgba::WHITE,
            "top-left goes to top-right after 90° CW"
        );
        RotateImage { quarter_turns: -1 }.apply(&mut doc).unwrap();
        assert_eq!((doc.width, doc.height), (40, 20));
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(0, 0),
            Rgba::WHITE
        );
        FlipImage { horizontal: true }.apply(&mut doc).unwrap();
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(39, 0),
            Rgba::WHITE
        );
        FlipImage { horizontal: false }.apply(&mut doc).unwrap();
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(39, 19),
            Rgba::WHITE
        );
        RotateImage { quarter_turns: 2 }.apply(&mut doc).unwrap();
        assert_eq!(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(0, 0),
            Rgba::WHITE
        );
    }

    #[test]
    fn pressure_zero_paints_nothing() {
        let mut doc = Document::new(20, 20);
        let id = doc.add_pixel_layer("p");
        let stroke = PaintStroke {
            layer: id,
            brush: Brush::default(),
            points: vec![StrokePoint::new(10.0, 10.0, 0.0)],
        };
        stroke.apply(&mut doc).unwrap();
        assert!(doc.layer(id).unwrap().pixels().unwrap().is_empty());
    }
}
