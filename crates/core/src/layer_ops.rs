//! Photoshop-standard layer operations: duplicate, merge down (with its
//! "merge group" and "merge clipping mask" forms), merge visible, flatten
//! and stamp visible.
//!
//! Every merge renders through the reference compositor
//! (`lumenply_render::composite_layers`), so merged pixels are exactly what
//! the screen showed for that part of the stack. Where Photoshop's rules
//! would change the picture and the maths allows otherwise, the picture
//! wins: merging down onto a Normal layer bakes the lower layer's opacity
//! into the pixels (the result is 100 %), instead of re-applying it to the
//! upper layer's content too.

use lumenply_doc::{BlendMode, Document, Layer, LayerContent, LayerId};
use lumenply_tiles::{Rect, Rgba, Tile, TileStore, TILE_SIZE};

use crate::{Command, EditError, EditResult};

// ---- helpers -------------------------------------------------------------------------

/// Canvas area a layer can paint: its raster bounds (tile-aligned, cheap)
/// grown by its effects' reach; for groups the union of the children.
/// `None` for layers that have no extent of their own (adjustments and
/// live filters act on whatever is below them).
pub fn layer_extent(l: &Layer) -> Option<Rect> {
    let own = match &l.content {
        LayerContent::Group(children) => children
            .iter()
            .filter_map(layer_extent)
            .reduce(|a, b| a.union(&b)),
        _ => l.raster_store().and_then(|s| s.bounds()),
    }?;
    let pad = if l.effects.is_empty() { 0 } else { l.effects.pad() };
    Some(Rect::new(
        own.x - pad,
        own.y - pad,
        own.w + 2 * pad as u32,
        own.h + 2 * pad as u32,
    ))
}

/// The area to composite when flattening `layers`: the canvas plus any
/// content hanging off it (pixels kept outside after a crop survive merges
/// that keep layers, as they do elsewhere).
fn render_area(layers: &[Layer], canvas: Rect) -> Rect {
    layers
        .iter()
        .filter_map(layer_extent)
        .fold(canvas, |acc, r| acc.union(&r))
}

/// Composite `layers` (bottom-to-top) into a fresh pixel store.
fn flatten(layers: &[Layer], area: Rect, canvas: Rect) -> TileStore {
    let mut out = lumenply_render::composite_layers(layers, area, canvas);
    out.prune_blank();
    out
}

/// A plain pixel layer carrying `store`, with the identity (id, name,
/// visibility, clip flag) of `like`.
fn pixel_like(like: &Layer, store: TileStore) -> Layer {
    let mut l = Layer::with_content(like.id, like.name.clone(), LayerContent::Pixel(store));
    l.visible = like.visible;
    l.clip = like.clip;
    l.locks = like.locks;
    l
}

fn siblings(doc: &Document, id: LayerId) -> Option<&[Layer]> {
    match doc.parent_of(id) {
        None => doc.layer(id).map(|_| doc.layers()),
        Some(p) => doc.layer(p)?.children(),
    }
}

/// Can `l` be the base of a clip chain (see the compositor)?
fn clip_baseable(l: &Layer) -> bool {
    !l.clip
        && matches!(
            l.content,
            LayerContent::Pixel(_)
                | LayerContent::Text(_)
                | LayerContent::Smart(_)
                | LayerContent::Shape(_)
                | LayerContent::Group(_)
        )
}

// ---- duplicate -----------------------------------------------------------------------

/// "Name copy", or "Name copy 2", "Name copy 3"... — the first name not
/// already used in `doc`, built on `name` without any copy suffix (so
/// duplicating "Sky copy" gives "Sky copy 2", as in Photoshop).
pub fn copy_name(doc: &Document, name: &str) -> String {
    let base = match name.rfind(" copy") {
        Some(i) => {
            let tail = &name[i + 5..];
            if tail.is_empty() || (tail.starts_with(' ') && tail[1..].chars().all(|c| c.is_ascii_digit())) {
                &name[..i]
            } else {
                name
            }
        }
        None => name,
    };
    let mut taken = std::collections::HashSet::new();
    doc.for_each_layer(|l| {
        taken.insert(l.name.clone());
    });
    let first = format!("{base} copy");
    if !taken.contains(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{base} copy {n}"))
        .find(|c| !taken.contains(c))
        .expect("unbounded")
}

/// Give `layer` and every descendant fresh ids, root first, so the root
/// copy takes [`Document::next_id`] as it was before the call.
fn reassign_ids(doc: &mut Document, layer: &mut Layer) {
    layer.id = doc.alloc_id();
    if let Some(children) = layer.children_mut() {
        for c in children {
            reassign_ids(doc, c);
        }
    }
}

/// Duplicate a layer of any kind directly above itself: groups are copied
/// deeply with fresh ids; masks, effects, clipping, text and smart-object
/// sources come along (tiles are shared copy-on-write, so this is cheap).
/// The copy's id is the document's `next_id()` before the command runs.
pub struct DuplicateLayer {
    pub layer: LayerId,
}

impl Command for DuplicateLayer {
    fn label(&self) -> String {
        "Duplicate layer".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        // The copy paints over the original's footprint.
        let l = doc.layer(self.layer)?;
        if l.children().is_some() {
            return None; // a pass-through group can reach below itself
        }
        layer_extent(l).map(|r| r.intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let src = doc.layer(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let mut copy = src.clone();
        copy.name = copy_name(doc, &src.name);
        reassign_ids(doc, &mut copy);
        let list = doc
            .siblings_mut(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?;
        let i = list.iter().position(|l| l.id == self.layer).expect("in siblings");
        list.insert(i + 1, copy);
        Ok(())
    }
}

// ---- merge down / merge group / merge clipping mask -------------------------------------

/// What Merge Down (Ctrl/Cmd+E) does for a layer, as Photoshop decides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeKind {
    /// Merge the layer into the one below it.
    Down,
    /// The layer is a group: flatten the group into one layer.
    Group,
    /// The layer is the base of a clipping group: merge the layers clipped
    /// to it into it.
    ClippingMask,
}

impl MergeKind {
    /// Menu label for this form of the command.
    pub fn label(self) -> &'static str {
        match self {
            MergeKind::Down => "Merge down",
            MergeKind::Group => "Merge group",
            MergeKind::ClippingMask => "Merge clipping mask",
        }
    }
}

/// What Merge Down would do with `id`, or why it can't run (a reason the
/// UI shows as is).
pub fn merge_down_kind(doc: &Document, id: LayerId) -> Result<MergeKind, &'static str> {
    let list = siblings(doc, id).ok_or("Select a layer first")?;
    let i = list
        .iter()
        .position(|l| l.id == id)
        .ok_or("Select a layer first")?;
    let l = &list[i];
    if !l.visible {
        return Err("The layer is hidden");
    }
    // Locks count with their enclosing groups' (see `locks`).
    let own = crate::locks::effective_locks(doc, id);
    if own.all {
        return Err("The layer is locked");
    }
    if l.children().is_some() {
        if own.pixels {
            return Err("The layer's pixels are locked");
        }
        return Ok(MergeKind::Group);
    }
    if clip_baseable(l) && list.get(i + 1).is_some_and(|above| above.clip) {
        if own.pixels || own.transparency {
            return Err("The layer's pixels are locked");
        }
        return Ok(MergeKind::ClippingMask);
    }
    let below = i
        .checked_sub(1)
        .map(|b| &list[b])
        .ok_or("Nothing below to merge into")?;
    if !below.visible {
        return Err("The layer below is hidden");
    }
    match below.content {
        LayerContent::Adjustment(_) => return Err("The layer below is an adjustment layer"),
        LayerContent::Filter(_) => return Err("The layer below is a live filter layer"),
        LayerContent::Group(_) => return Err("The layer below is a group; merge the group first"),
        _ => {}
    }
    // Merging paints onto the lower layer: its pixel and transparency
    // locks refuse that (its position lock doesn't: nothing moves).
    let under = crate::locks::effective_locks(doc, below.id);
    if under.pixels || under.transparency {
        return Err("The layer below is locked");
    }
    Ok(MergeKind::Down)
}

/// The range `[lo, hi)` of siblings that a merge of `list[i]` turns into
/// one layer at `lo`.
fn merge_range(list: &[Layer], i: usize, kind: MergeKind) -> (usize, usize) {
    match kind {
        MergeKind::Down => (i - 1, i + 1),
        MergeKind::Group => (i, i + 1),
        MergeKind::ClippingMask => {
            // The chain as the compositor reads it.
            let mut end = i + 1;
            while end < list.len() && list[end].clip && !matches!(list[end].content, LayerContent::Filter(_))
            {
                end += 1;
            }
            (i, end)
        }
    }
}

/// Merge Down, Photoshop's Ctrl/Cmd+E, in whichever form fits the layer
/// (see [`merge_down_kind`]):
///
/// * **Down** — the layer is composited onto the layer below with its own
///   blend mode, opacity, mask, effects and clipping; the result is a pixel
///   layer with the lower layer's id, name, position and clip flag. The
///   lower layer's mask and effects are baked in. A Normal lower layer's
///   opacity is baked too (the result is 100 % and looks unchanged); a
///   lower layer in another blend mode keeps that mode and opacity, and
///   the upper layer is composited onto its full-strength pixels.
///   An adjustment or live filter merged down is applied to the pixels.
/// * **Group** — the group's isolated composite becomes one pixel layer
///   keeping the group's name, blend mode (pass-through → Normal) and
///   opacity.
/// * **Clipping mask** — the base and the layers clipped to it composite
///   as the chain does on screen, into the base.
pub struct MergeDown {
    pub layer: LayerId,
}

impl Command for MergeDown {
    fn label(&self) -> String {
        "Merge layers".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        // Only the merged layers' footprint can change (and usually
        // nothing does: see the type's docs).
        let kind = merge_down_kind(doc, self.layer).ok()?;
        let list = siblings(doc, self.layer)?;
        let i = list.iter().position(|l| l.id == self.layer)?;
        let (lo, hi) = merge_range(list, i, kind);
        let mut acc: Option<Rect> = None;
        for l in &list[lo..hi] {
            let r = layer_extent(l)?;
            acc = Some(acc.map_or(r, |a| a.union(&r)));
        }
        acc.map(|r| r.intersect(&doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let kind = merge_down_kind(doc, self.layer).map_err(|why| EditError::Invalid(why.into()))?;
        let canvas = doc.canvas();
        let list = doc
            .siblings_mut(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?;
        let i = list.iter().position(|l| l.id == self.layer).expect("in siblings");
        let (lo, hi) = merge_range(list, i, kind);
        let base = &list[lo];
        // The base renders at full strength in Normal mode; its blend and
        // opacity go back on the result, so the unit looks as it did.
        // A Normal base merged down instead keeps its opacity in the
        // render and the result is 100 %, which is also what was shown.
        let keep_opacity_in_pixels = kind == MergeKind::Down && base.blend == BlendMode::Normal;
        let (blend, opacity) = if keep_opacity_in_pixels {
            (BlendMode::Normal, 1.0)
        } else if base.pass_through {
            (BlendMode::Normal, base.opacity)
        } else {
            (base.blend, base.opacity)
        };
        let mut unit: Vec<Layer> = list[lo..hi].to_vec();
        {
            let b = &mut unit[0];
            b.clip = false;
            b.visible = true;
            b.pass_through = false;
            if !keep_opacity_in_pixels {
                b.blend = BlendMode::Normal;
                b.opacity = 1.0;
            }
        }
        if kind == MergeKind::Down && list[lo].clip {
            // Both sit in someone else's clipping group: the upper layer
            // goes onto the lower one's pixels, and the result stays
            // clipped to that group's base.
            unit[1].clip = false;
        }
        let area = render_area(&unit, canvas);
        let store = flatten(&unit, area, canvas);
        let mut merged = pixel_like(&list[lo], store);
        merged.blend = blend;
        merged.opacity = opacity;
        list.splice(lo..hi, std::iter::once(merged));
        Ok(())
    }
}

// ---- merge selected ---------------------------------------------------------------------

/// Why merging the selected layers can't run, if it can't.
pub fn merge_selected_block(doc: &Document, ids: &[LayerId]) -> Option<&'static str> {
    if ids.len() < 2 {
        return Some("Select two or more layers");
    }
    if ids.iter().any(|&i| doc.layer(i).is_none()) {
        return Some("A selected layer no longer exists");
    }
    let parent = doc.parent_of(ids[0]);
    if ids.iter().any(|&i| doc.parent_of(i) != parent) {
        return Some("Select layers in the same group to merge them");
    }
    let visible = ids
        .iter()
        .filter(|&&i| doc.layer(i).is_some_and(|l| l.visible))
        .count();
    (visible < 2).then_some("Select two or more visible layers")
}

/// Photoshop's Merge Layers (Ctrl/Cmd+E with several layers selected):
/// the selected visible layers composite, in stack order, into one pixel
/// layer that takes the topmost one's place, id and name. Adjustments
/// among them apply to the selected layers below them only. Hidden
/// selected layers are left as they are.
pub struct MergeSelected {
    pub layers: Vec<LayerId>,
}

impl Command for MergeSelected {
    fn label(&self) -> String {
        "Merge layers".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if let Some(why) = merge_selected_block(doc, &self.layers) {
            return Err(EditError::Invalid(why.into()));
        }
        let canvas = doc.canvas();
        let list = doc
            .siblings_mut(self.layers[0])
            .ok_or(EditError::NoLayer(self.layers[0]))?;
        let chosen: Vec<usize> = (0..list.len())
            .filter(|&i| list[i].visible && self.layers.contains(&list[i].id))
            .collect();
        let mut unit: Vec<Layer> = chosen.iter().map(|&i| list[i].clone()).collect();
        // The lowest composites on its own: a clip to an unselected base
        // has nothing to clip to inside the merge.
        unit[0].clip = false;
        let store = flatten(&unit, render_area(&unit, canvas), canvas);
        let top = *chosen.last().expect("two or more");
        let mut merged = pixel_like(&list[top], store);
        merged.clip = false;
        let merged_id = merged.id;
        let old = std::mem::take(list);
        for (i, l) in old.into_iter().enumerate() {
            if l.id == merged_id {
                list.push(merged.clone());
            } else if !chosen.contains(&i) {
                list.push(l);
            }
        }
        Ok(())
    }
}

// ---- merge visible / stamp visible / flatten --------------------------------------------

/// Why Merge Visible can't run, if it can't.
pub fn merge_visible_block(doc: &Document) -> Option<&'static str> {
    let visible = doc.layers().iter().filter(|l| l.visible).count();
    match visible {
        0 => Some("Nothing is visible"),
        1 => Some("Only one layer is visible"),
        _ => None,
    }
}

/// Merge every visible top-level layer (groups included) into one pixel
/// layer, which takes the place, id and name of the lowest visible one.
/// Hidden layers stay where they were. The merged layer holds exactly the
/// composite the screen showed.
pub struct MergeVisible;

impl Command for MergeVisible {
    fn label(&self) -> String {
        "Merge visible".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if let Some(why) = merge_visible_block(doc) {
            return Err(EditError::Invalid(why.into()));
        }
        let canvas = doc.canvas();
        let layers = doc.layers();
        let store = flatten(layers, render_area(layers, canvas), canvas);
        let first = layers.iter().find(|l| l.visible).expect("checked above");
        let mut merged = pixel_like(first, store);
        merged.clip = false;
        let first_id = first.id;
        let old = std::mem::take(doc.layers_mut());
        let mut out = Vec::with_capacity(old.len());
        for l in old {
            if l.id == first_id {
                out.push(merged.clone());
            } else if !l.visible {
                out.push(l);
            }
        }
        *doc.layers_mut() = out;
        Ok(())
    }
}

/// Why Stamp Visible can't run, if it can't.
pub fn stamp_visible_block(doc: &Document) -> Option<&'static str> {
    (!doc.layers().iter().any(|l| l.visible)).then_some("Nothing is visible")
}

/// Photoshop's "stamp visible" (Shift+Alt+Ctrl/Cmd+E): a new pixel layer
/// on top of the stack holding the composite of everything visible; the
/// layers themselves are left as they are. Its id is the document's
/// `next_id()` before the command runs.
pub struct StampVisible {
    pub name: String,
}

impl Command for StampVisible {
    fn label(&self) -> String {
        "Stamp visible".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if let Some(why) = stamp_visible_block(doc) {
            return Err(EditError::Invalid(why.into()));
        }
        let canvas = doc.canvas();
        let store = flatten(doc.layers(), render_area(doc.layers(), canvas), canvas);
        let id = doc.alloc_id();
        let name = if self.name.trim().is_empty() {
            "Stamp".to_string()
        } else {
            self.name.trim().to_string()
        };
        doc.add_layer(Layer::with_content(id, name, LayerContent::Pixel(store)));
        Ok(())
    }
}

/// Flatten the image: every visible layer composited over white into one
/// opaque, canvas-sized pixel layer named "Background"; hidden layers are
/// discarded, as in Photoshop. Its id is the document's `next_id()` before
/// the command runs.
pub struct FlattenImage;

impl Command for FlattenImage {
    fn label(&self) -> String {
        "Flatten image".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if doc.layers().is_empty() {
            return Err(EditError::Invalid("There are no layers to flatten".into()));
        }
        let canvas = doc.canvas();
        let comp = lumenply_render::composite_layers(doc.layers(), canvas, canvas);
        let mut store = TileStore::new();
        for c in canvas.tiles() {
            let inside = c.rect().intersect(&canvas);
            let mut t = Tile::filled(Rgba::WHITE);
            let src = comp.tile(c).map(|t| t.pixels());
            let (ox, oy) = c.origin();
            let full = inside == c.rect();
            for (i, p) in t.pixels_mut().iter_mut().enumerate() {
                let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
                if !full && !inside.contains(x, y) {
                    *p = Rgba::TRANSPARENT;
                    continue;
                }
                if let Some(s) = &src {
                    *p = s[i].over(Rgba::WHITE);
                }
            }
            store.insert(c, std::sync::Arc::new(t));
        }
        let id = doc.alloc_id();
        let bg = Layer::with_content(id, "Background", LayerContent::Pixel(store));
        *doc.layers_mut() = vec![bg];
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::*;
    use crate::Editor;
    use lumenply_doc::{Adjustment, LayerEffects, Mask, ShadowFx, TextLayer};
    use lumenply_tiles::Raster;

    fn solid(w: u32, h: u32, c: [f32; 4]) -> Raster {
        Raster::filled(w, h, Rgba::from_straight(c[0], c[1], c[2], c[3]))
    }

    fn add(ed: &mut Editor, name: &str, r: Raster, x: i32, y: i32) -> LayerId {
        let id = ed.doc().next_id();
        ed.execute(&AddPixelLayer::from_raster(name, r, x, y)).unwrap();
        id
    }

    fn max_diff(a: &Raster, b: &Raster) -> f32 {
        a.pixels
            .iter()
            .zip(&b.pixels)
            .map(|(p, q)| {
                (p.r - q.r)
                    .abs()
                    .max((p.g - q.g).abs())
                    .max((p.b - q.b).abs())
                    .max((p.a - q.a).abs())
            })
            .fold(0.0, f32::max)
    }

    fn close(p: Rgba, want: [f32; 4]) -> bool {
        (p.r - want[0]).abs() < 1e-3
            && (p.g - want[1]).abs() < 1e-3
            && (p.b - want[2]).abs() < 1e-3
            && (p.a - want[3]).abs() < 1e-3
    }

    #[test]
    fn merge_selected_composites_just_the_chosen_layers() {
        // Bottom to top: grey base, red (selected), blue unselected,
        // green 50% (selected). Merging red + green leaves base and blue.
        let mut ed = Editor::new(Document::new(8, 8));
        let base = add(&mut ed, "base", solid(8, 8, [0.5, 0.5, 0.5, 1.0]), 0, 0);
        let red = add(&mut ed, "red", solid(4, 4, [1.0, 0.0, 0.0, 1.0]), 0, 0);
        let blue = add(&mut ed, "blue", solid(2, 2, [0.0, 0.0, 1.0, 1.0]), 6, 6);
        let green = add(&mut ed, "green", solid(4, 4, [0.0, 1.0, 0.0, 1.0]), 2, 2);
        ed.execute(&SetOpacity {
            layer: green,
            opacity: 0.5,
        })
        .unwrap();
        let before = lumenply_render::composite_raster(ed.doc());
        ed.execute(&MergeSelected {
            layers: vec![red, green],
        })
        .unwrap();
        let ids: Vec<_> = ed.doc().layers().iter().map(|l| l.id).collect();
        assert_eq!(
            ids,
            vec![base, blue, green],
            "merged into the topmost's slot and id"
        );
        let merged = ed.doc().layer(green).unwrap();
        assert_eq!(merged.name, "green");
        assert!(merged.pixels().is_some());
        assert!(
            (merged.opacity - 1.0).abs() < 1e-6,
            "opacity baked into the pixels"
        );
        // Red under 50% green, alone: (0.5, 0.5, 0, 1) where they overlap.
        assert!(close(
            merged.pixels().unwrap().get_pixel(3, 3),
            [0.5, 0.5, 0.0, 1.0]
        ));
        // The picture didn't change.
        let after = lumenply_render::composite_raster(ed.doc());
        assert!(max_diff(&before, &after) < 2e-4);
    }

    #[test]
    fn merge_selected_needs_two_visible_siblings() {
        let mut ed = Editor::new(Document::new(8, 8));
        let a = add(&mut ed, "a", solid(4, 4, [1.0, 0.0, 0.0, 1.0]), 0, 0);
        let b = add(&mut ed, "b", solid(4, 4, [0.0, 1.0, 0.0, 1.0]), 0, 0);
        assert_eq!(
            merge_selected_block(ed.doc(), &[a]),
            Some("Select two or more layers")
        );
        ed.execute(&SetVisible {
            layer: b,
            visible: false,
        })
        .unwrap();
        assert_eq!(
            merge_selected_block(ed.doc(), &[a, b]),
            Some("Select two or more visible layers")
        );
        ed.execute(&GroupLayers {
            layers: vec![b],
            name: "g".into(),
        })
        .unwrap();
        assert_eq!(
            merge_selected_block(ed.doc(), &[a, b]),
            Some("Select layers in the same group to merge them")
        );
    }

    #[test]
    fn copy_names_follow_photoshop() {
        let mut d = Document::new(4, 4);
        d.add_pixel_layer("Sky");
        assert_eq!(copy_name(&d, "Sky"), "Sky copy");
        d.add_pixel_layer("Sky copy");
        assert_eq!(copy_name(&d, "Sky"), "Sky copy 2");
        assert_eq!(copy_name(&d, "Sky copy"), "Sky copy 2");
        d.add_pixel_layer("Sky copy 2");
        assert_eq!(copy_name(&d, "Sky copy 2"), "Sky copy 3");
        // "copy" inside a name is not a suffix.
        assert_eq!(copy_name(&d, "Sky copyright"), "Sky copyright copy");
    }

    #[test]
    fn duplicate_copies_any_layer_directly_above_with_fresh_ids() {
        let mut ed = Editor::new(Document::new(64, 64));
        let bg = add(&mut ed, "Bg", solid(64, 64, [0.2, 0.4, 0.6, 1.0]), 0, 0);
        // A group holding a masked, styled pixel layer and a text layer.
        let a = add(&mut ed, "A", solid(10, 10, [1.0, 0.0, 0.0, 1.0]), 5, 5);
        ed.execute(&SetMask {
            layer: a,
            mask: Some(Mask::hide_all()),
        })
        .unwrap();
        let fx = LayerEffects {
            drop_shadow: Some(ShadowFx::default()),
            ..LayerEffects::default()
        };
        ed.execute(&SetLayerEffects {
            layer: a,
            effects: fx.clone(),
        })
        .unwrap();
        let t = ed.doc().next_id();
        ed.execute(&AddTextLayer {
            text: TextLayer::new("Hi", 4.0, 40.0, 20.0, [1.0, 1.0, 1.0, 1.0]),
            above: None,
        })
        .unwrap();
        let g = ed.doc().next_id();
        ed.execute(&GroupLayers {
            layers: vec![a, t],
            name: "G".into(),
        })
        .unwrap();
        let before = ed.doc().layer_count();

        let copy = ed.doc().next_id();
        ed.execute(&DuplicateLayer { layer: g }).unwrap();
        let d = ed.doc();
        assert_eq!(d.layer_count(), before + 3, "group + two children");
        let ids: Vec<LayerId> = d.layers().iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![bg, g, copy], "the copy sits directly above");
        let c = d.layer(copy).unwrap();
        assert_eq!(c.name, "G copy");
        let kids = c.children().unwrap();
        assert_eq!(kids.len(), 2);
        assert_eq!(
            (kids[0].id, kids[1].id),
            (copy + 1, copy + 2),
            "fresh ids, root first"
        );
        assert_eq!(kids[0].name, "A", "children keep their names");
        assert!(kids[0].mask.as_ref().is_some_and(|m| m.default == 0.0));
        assert_eq!(kids[0].effects, fx);
        assert_eq!(kids[1].text_layer().unwrap().text, "Hi");
        assert!(close(
            kids[0].pixels().unwrap().get_pixel(6, 6),
            [1.0, 0.0, 0.0, 1.0]
        ));
        // Originals untouched; every id in the tree unique.
        assert!(d.layer(a).is_some() && d.layer(t).is_some());
        let mut all = Vec::new();
        d.for_each_layer(|l| all.push(l.id));
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n);
        assert_eq!(d.next_id(), copy + 3);

        // Undo removes the whole copy.
        ed.undo();
        assert_eq!(ed.doc().layer_count(), before);
        assert!(ed.doc().layer(copy).is_none());
    }

    #[test]
    fn duplicating_a_nested_layer_stays_in_its_group() {
        let mut ed = Editor::new(Document::new(8, 8));
        let a = add(&mut ed, "A", solid(2, 2, [1.0, 1.0, 1.0, 1.0]), 0, 0);
        let b = add(&mut ed, "B", solid(2, 2, [1.0, 1.0, 1.0, 1.0]), 0, 0);
        let g = ed.doc().next_id();
        ed.execute(&GroupLayers {
            layers: vec![a, b],
            name: "G".into(),
        })
        .unwrap();
        let copy = ed.doc().next_id();
        ed.execute(&DuplicateLayer { layer: a }).unwrap();
        let kids: Vec<LayerId> = ed
            .doc()
            .layer(g)
            .unwrap()
            .children()
            .unwrap()
            .iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(kids, vec![a, copy, b]);
        assert_eq!(ed.doc().layer(copy).unwrap().name, "A copy");
    }

    /// Bottom: opaque (0.2, 0.4, 0.6) 64×64. Top: a red 20×20 square at
    /// (10, 10).
    fn two_layers() -> (Editor, LayerId, LayerId) {
        let mut ed = Editor::new(Document::new(64, 64));
        let b = add(&mut ed, "Below", solid(64, 64, [0.2, 0.4, 0.6, 1.0]), 0, 0);
        let a = add(&mut ed, "Above", solid(20, 20, [1.0, 0.0, 0.0, 1.0]), 10, 10);
        (ed, b, a)
    }

    #[test]
    fn merge_down_composites_with_blend_opacity_mask_and_effects() {
        let (mut ed, b, a) = two_layers();
        ed.execute(&SetOpacity {
            layer: a,
            opacity: 0.5,
        })
        .unwrap();
        // The mask hides the right half of the square.
        let mut m = Mask::reveal_all();
        for y in 0..64 {
            for x in 20..64 {
                m.set_value(x, y, 0.0);
            }
        }
        ed.execute(&SetMask {
            layer: a,
            mask: Some(m),
        })
        .unwrap();
        ed.execute(&SetLayerEffects {
            layer: a,
            effects: LayerEffects {
                // Cast well away from the square, so the pixels checked
                // below have no shadow under them.
                drop_shadow: Some(ShadowFx {
                    dx: 30.0,
                    dy: 30.0,
                    blur: 2.0,
                    color: [0.0, 0.0, 0.0],
                    opacity: 0.8,
                }),
                ..LayerEffects::default()
            },
        })
        .unwrap();
        ed.execute(&SetBlendMode {
            layer: a,
            blend: BlendMode::Screen,
        })
        .unwrap();
        let before = lumenply_render::composite_raster(ed.doc());

        assert_eq!(merge_down_kind(ed.doc(), a), Ok(MergeKind::Down));
        ed.execute(&MergeDown { layer: a }).unwrap();
        let d = ed.doc();
        assert_eq!(d.layers().len(), 1);
        let m = &d.layers()[0];
        assert_eq!((m.id, m.name.as_str()), (b, "Below"), "keeps the lower layer");
        assert!(m.pixels().is_some() && m.mask.is_none() && m.effects.is_empty());
        assert_eq!((m.blend, m.opacity), (BlendMode::Normal, 1.0));
        let after = lumenply_render::composite_raster(d);
        assert!(max_diff(&before, &after) < 2e-4, "{}", max_diff(&before, &after));
        // Screen at 50 %: 0.5·(0.2,0.4,0.6) + 0.5·screen(…, red) =
        // (0.2+0.8·0.5, 0.4, 0.6) = (0.6, 0.4, 0.6) inside the mask.
        assert!(
            close(after.get(12, 12), [0.6, 0.4, 0.6, 1.0]),
            "{:?}",
            after.get(12, 12)
        );
        // Masked-out half: just the bottom layer.
        assert!(close(after.get(25, 12), [0.2, 0.4, 0.6, 1.0]));
        // The shadow lands around (40..60, 40..60): black at 80 %, under
        // a layer at 50 %, darkens the bottom by 40 %.
        assert!(
            close(after.get(45, 50), [0.12, 0.24, 0.36, 1.0]),
            "{:?}",
            after.get(45, 50)
        );
        assert!(close(after.get(5, 50), [0.2, 0.4, 0.6, 1.0]));
        // Undo restores both layers.
        ed.undo();
        assert_eq!(ed.doc().layers().len(), 2);
    }

    #[test]
    fn merge_down_bakes_a_normal_lower_layers_opacity() {
        let (mut ed, b, a) = two_layers();
        ed.execute(&SetOpacity {
            layer: b,
            opacity: 0.5,
        })
        .unwrap();
        ed.execute(&MergeDown { layer: a }).unwrap();
        let m = ed.doc().layer(b).unwrap();
        assert_eq!((m.blend, m.opacity), (BlendMode::Normal, 1.0));
        let px = m.pixels().unwrap();
        // Red square at full strength; elsewhere the lower layer at 50 %
        // (premultiplied).
        assert!(close(px.get_pixel(15, 15), [1.0, 0.0, 0.0, 1.0]));
        assert!(close(px.get_pixel(40, 40), [0.1, 0.2, 0.3, 0.5]));
    }

    #[test]
    fn merge_down_keeps_a_lower_layers_blend_mode_and_opacity() {
        let (mut ed, b, a) = two_layers();
        ed.execute(&SetBlendMode {
            layer: b,
            blend: BlendMode::Multiply,
        })
        .unwrap();
        ed.execute(&SetOpacity {
            layer: b,
            opacity: 0.7,
        })
        .unwrap();
        ed.execute(&MergeDown { layer: a }).unwrap();
        let m = ed.doc().layer(b).unwrap();
        assert_eq!((m.blend, m.opacity), (BlendMode::Multiply, 0.7));
        // The pixels are the full-strength lower layer with red over it.
        let px = m.pixels().unwrap();
        assert!(close(px.get_pixel(15, 15), [1.0, 0.0, 0.0, 1.0]));
        assert!(close(px.get_pixel(40, 40), [0.2, 0.4, 0.6, 1.0]));
    }

    #[test]
    fn merging_an_adjustment_down_applies_it() {
        let (mut ed, b, _a) = two_layers();
        let inv = ed.doc().next_id();
        ed.execute(&AddAdjustmentLayer::new(Adjustment::Invert)).unwrap();
        ed.execute(&MergeDown { layer: inv }).unwrap();
        let d = ed.doc();
        assert_eq!(d.layers().len(), 2);
        assert!(d.layer(inv).is_none());
        // Invert runs on gamma-encoded values (ADR 0005); compare with the
        // reference compositor's answer for the same stack.
        let mut want = Document::new(64, 64);
        want.add_pixel_layer("x");
        *want.layers_mut()[0].pixels_mut().unwrap() =
            TileStore::from_raster(&solid(1, 1, [1.0, 0.0, 0.0, 1.0]), 0, 0);
        want.add_adjustment(Adjustment::Invert);
        let red_inverted = lumenply_render::composite_raster(&want).get(0, 0);
        let got = d.layers()[1].pixels().unwrap().get_pixel(15, 15);
        assert!(close(got, [red_inverted.r, red_inverted.g, red_inverted.b, 1.0]));
        // Inverted red is cyan: no red left, full green and blue.
        assert!(close(got, [0.0, 1.0, 1.0, 1.0]), "{got:?}");
        assert!(d.layer(b).is_some());
    }

    #[test]
    fn merge_down_is_blocked_with_reasons() {
        let (mut ed, b, a) = two_layers();
        assert_eq!(merge_down_kind(ed.doc(), b), Err("Nothing below to merge into"));
        ed.execute(&SetVisible {
            layer: b,
            visible: false,
        })
        .unwrap();
        assert_eq!(merge_down_kind(ed.doc(), a), Err("The layer below is hidden"));
        ed.execute(&SetVisible {
            layer: b,
            visible: true,
        })
        .unwrap();
        ed.execute(&SetVisible {
            layer: a,
            visible: false,
        })
        .unwrap();
        assert_eq!(merge_down_kind(ed.doc(), a), Err("The layer is hidden"));
        ed.execute(&SetVisible {
            layer: a,
            visible: true,
        })
        .unwrap();

        let adj = ed.doc().next_id();
        ed.execute(&AddAdjustmentLayer::new(Adjustment::Invert)).unwrap();
        let top = add(&mut ed, "Top", solid(4, 4, [1.0, 1.0, 1.0, 1.0]), 0, 0);
        assert_eq!(
            merge_down_kind(ed.doc(), top),
            Err("The layer below is an adjustment layer")
        );
        let err = ed.execute(&MergeDown { layer: top }).unwrap_err();
        assert_eq!(err.to_string(), "The layer below is an adjustment layer");
        ed.execute(&RemoveLayer { layer: adj }).unwrap();
        let f = ed.doc().next_id();
        ed.execute(&AddFilterLayer {
            filter: lumenply_doc::Filter::GaussianBlur { radius: 2.0 },
            opacity: 1.0,
            above: Some(a),
        })
        .unwrap();
        assert_eq!(
            merge_down_kind(ed.doc(), top),
            Err("The layer below is a live filter layer")
        );
        ed.execute(&RemoveLayer { layer: f }).unwrap();
        let g = ed.doc().next_id();
        ed.execute(&GroupLayers {
            layers: vec![a],
            name: "G".into(),
        })
        .unwrap();
        assert_eq!(
            merge_down_kind(ed.doc(), top),
            Err("The layer below is a group; merge the group first")
        );
        assert_eq!(merge_down_kind(ed.doc(), g), Ok(MergeKind::Group));
    }

    #[test]
    fn merge_group_flattens_the_group_and_keeps_its_blend_and_opacity() {
        let (mut ed, b, a) = two_layers();
        let inv = ed.doc().next_id();
        ed.execute(&AddAdjustmentLayer::new(Adjustment::Invert)).unwrap();
        let g = ed.doc().next_id();
        ed.execute(&GroupLayers {
            layers: vec![a, inv],
            name: "Group 1".into(),
        })
        .unwrap();
        ed.execute(&SetOpacity {
            layer: g,
            opacity: 0.4,
        })
        .unwrap();
        let before = lumenply_render::composite_raster(ed.doc());
        ed.execute(&MergeDown { layer: g }).unwrap();
        let d = ed.doc();
        let m = d.layer(g).unwrap();
        assert_eq!(m.name, "Group 1");
        assert!(m.pixels().is_some());
        assert_eq!((m.blend, m.opacity), (BlendMode::Normal, 0.4));
        // The isolated invert only touched the group's own red square.
        assert!(close(m.pixels().unwrap().get_pixel(15, 15), [0.0, 1.0, 1.0, 1.0]));
        assert!(m.pixels().unwrap().get_pixel(40, 40).a == 0.0);
        let after = lumenply_render::composite_raster(d);
        assert!(max_diff(&before, &after) < 2e-4);
        assert!(d.layer(b).is_some() && d.layer(a).is_none());
    }

    #[test]
    fn merge_clipping_mask_merges_the_chain_into_its_base() {
        let mut ed = Editor::new(Document::new(64, 64));
        let bg = add(&mut ed, "Bg", solid(64, 64, [1.0, 1.0, 1.0, 1.0]), 0, 0);
        let base = add(&mut ed, "Shape", solid(20, 20, [0.0, 0.0, 1.0, 1.0]), 10, 10);
        let clipped = add(&mut ed, "Paint", solid(64, 20, [0.0, 1.0, 0.0, 1.0]), 0, 0);
        ed.execute(&SetClipped {
            layer: clipped,
            clip: true,
        })
        .unwrap();
        ed.execute(&SetBlendMode {
            layer: base,
            blend: BlendMode::Multiply,
        })
        .unwrap();
        let before = lumenply_render::composite_raster(ed.doc());
        assert_eq!(merge_down_kind(ed.doc(), base), Ok(MergeKind::ClippingMask));
        ed.execute(&MergeDown { layer: base }).unwrap();
        let d = ed.doc();
        assert_eq!(
            d.layers().iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![bg, base]
        );
        let m = d.layer(base).unwrap();
        assert_eq!(m.blend, BlendMode::Multiply);
        let px = m.pixels().unwrap();
        // Green where the paint overlaps the shape, blue below the paint,
        // nothing outside the shape.
        assert!(close(px.get_pixel(15, 15), [0.0, 1.0, 0.0, 1.0]));
        assert!(close(px.get_pixel(15, 25), [0.0, 0.0, 1.0, 1.0]));
        assert_eq!(px.get_pixel(5, 5).a, 0.0);
        let after = lumenply_render::composite_raster(d);
        assert!(max_diff(&before, &after) < 2e-4);
    }

    #[test]
    fn merge_visible_equals_the_composite_and_keeps_hidden_layers() {
        let (mut ed, b, a) = two_layers();
        let hidden = add(&mut ed, "Hidden", solid(64, 64, [0.0, 1.0, 0.0, 1.0]), 0, 0);
        ed.execute(&SetVisible {
            layer: hidden,
            visible: false,
        })
        .unwrap();
        let top = add(&mut ed, "Top", solid(8, 8, [0.0, 0.0, 0.0, 0.5]), 30, 30);
        ed.execute(&SetBlendMode {
            layer: top,
            blend: BlendMode::Multiply,
        })
        .unwrap();
        let before = lumenply_render::composite_raster(ed.doc());
        ed.execute(&MergeVisible).unwrap();
        let d = ed.doc();
        let ids: Vec<LayerId> = d.layers().iter().map(|l| l.id).collect();
        assert_eq!(
            ids,
            vec![b, hidden],
            "merged at the lowest visible slot; hidden kept"
        );
        assert_eq!(d.layer(b).unwrap().name, "Below");
        assert!(d.layer(a).is_none() && d.layer(top).is_none());
        let after = lumenply_render::composite_raster(d);
        assert!(max_diff(&before, &after) < 2e-4);
        assert!(close(after.get(15, 15), [1.0, 0.0, 0.0, 1.0]));
        // Black at 50 % multiplied over (0.2, 0.4, 0.6): half of each.
        assert!(
            close(after.get(32, 32), [0.1, 0.2, 0.3, 1.0]),
            "{:?}",
            after.get(32, 32)
        );

        let mut solo = Editor::new(Document::new(8, 8));
        add(&mut solo, "Only", solid(8, 8, [1.0, 1.0, 1.0, 1.0]), 0, 0);
        assert_eq!(merge_visible_block(solo.doc()), Some("Only one layer is visible"));
        assert!(solo.execute(&MergeVisible).is_err());
    }

    #[test]
    fn flatten_composites_over_white_and_drops_hidden_layers() {
        let mut ed = Editor::new(Document::new(300, 40));
        add(&mut ed, "Half red", solid(300, 40, [1.0, 0.0, 0.0, 0.5]), 0, 0);
        let hidden = add(&mut ed, "Hidden", solid(300, 40, [0.0, 1.0, 0.0, 1.0]), 0, 0);
        ed.execute(&SetVisible {
            layer: hidden,
            visible: false,
        })
        .unwrap();
        // Pixels hanging off the canvas are cropped away.
        add(
            &mut ed,
            "Off canvas",
            solid(10, 10, [0.0, 0.0, 1.0, 1.0]),
            295,
            35,
        );
        let id = ed.doc().next_id();
        ed.execute(&FlattenImage).unwrap();
        let d = ed.doc();
        assert_eq!(d.layers().len(), 1);
        let l = &d.layers()[0];
        assert_eq!((l.id, l.name.as_str()), (id, "Background"));
        let px = l.pixels().unwrap();
        // 50 % red over white, premultiplied linear: (1, 0.5, 0.5, 1).
        assert!(
            close(px.get_pixel(5, 5), [1.0, 0.5, 0.5, 1.0]),
            "{:?}",
            px.get_pixel(5, 5)
        );
        // The second tile column (x ≥ 256) is opaque inside the canvas.
        assert!(close(px.get_pixel(280, 20), [1.0, 0.5, 0.5, 1.0]));
        assert!(close(px.get_pixel(297, 37), [0.0, 0.0, 1.0, 1.0]));
        assert_eq!(px.get_pixel(300, 5).a, 0.0, "nothing right of the canvas");
        assert_eq!(px.get_pixel(5, 40).a, 0.0, "nothing below the canvas");
        assert_eq!(px.content_bounds(), Some(d.canvas()));
    }

    #[test]
    fn stamp_visible_adds_the_composite_on_top() {
        let (mut ed, b, a) = two_layers();
        ed.execute(&SetOpacity {
            layer: a,
            opacity: 0.25,
        })
        .unwrap();
        let before = lumenply_render::composite_raster(ed.doc());
        let id = ed.doc().next_id();
        ed.execute(&StampVisible { name: "Stamp".into() }).unwrap();
        let d = ed.doc();
        assert_eq!(
            d.layers().iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![b, a, id]
        );
        let s = d.layer(id).unwrap();
        assert_eq!(s.name, "Stamp");
        let flat = s.pixels().unwrap().to_raster(d.canvas());
        assert!(max_diff(&before, &flat) < 2e-4);
        // A quarter of red over (0.2, 0.4, 0.6).
        assert!(
            close(flat.get(15, 15), [0.4, 0.3, 0.45, 1.0]),
            "{:?}",
            flat.get(15, 15)
        );
        let mut empty = Editor::new(Document::new(4, 4));
        assert_eq!(stamp_visible_block(empty.doc()), Some("Nothing is visible"));
        assert!(empty.execute(&StampVisible { name: String::new() }).is_err());
    }

    #[test]
    fn merging_respects_locks() {
        use crate::locks::SetLayerLocks;
        use lumenply_doc::LayerLocks;
        let (mut ed, b, a) = two_layers();
        for (locks, why) in [
            (
                LayerLocks {
                    transparency: true,
                    ..LayerLocks::NONE
                },
                Some("The layer below is locked"),
            ),
            (
                LayerLocks {
                    pixels: true,
                    ..LayerLocks::NONE
                },
                Some("The layer below is locked"),
            ),
            (
                LayerLocks {
                    position: true,
                    ..LayerLocks::NONE
                },
                None,
            ),
        ] {
            ed.execute(&SetLayerLocks { layer: b, locks }).unwrap();
            assert_eq!(merge_down_kind(ed.doc(), a).err(), why, "{locks:?}");
        }
        ed.execute(&SetLayerLocks {
            layer: a,
            locks: LayerLocks {
                all: true,
                ..LayerLocks::NONE
            },
        })
        .unwrap();
        assert_eq!(merge_down_kind(ed.doc(), a), Err("The layer is locked"));
        // Whole-document merges are not limited by locks, as in Photoshop.
        ed.execute(&MergeVisible).unwrap();
        assert_eq!(ed.doc().layers().len(), 1);
        assert!(
            ed.doc().layer(b).unwrap().locks.position,
            "the merged layer keeps the lower one's locks"
        );
    }
}
