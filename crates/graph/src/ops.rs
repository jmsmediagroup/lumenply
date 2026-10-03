//! The operations a node can perform: a type, its parameters (serialised as
//! JSON next to the type) and named input ports. Each op evaluates one
//! output tile at a time from the tiles of its inputs; the compositing ops
//! hand those tiles to the reference compositor in `lumenply-render`, so a
//! graph renders exactly what the layer tree it came from renders.

use std::sync::{Arc, OnceLock};

use lumenply_doc::{Adjustment, BlendMode, Filter, Layer, LayerContent, LayerEffects, Mask};
use lumenply_tiles::{Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use serde::{Deserialize, Serialize};

use crate::blob::BlobId;
use crate::eval::Ctx;
use crate::model::Node;

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_normal(m: &BlendMode) -> bool {
    *m == BlendMode::Normal
}

/// How a layer composites onto what is below it: everything about a layer
/// except its content and mask, which arrive through input ports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerProps {
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f32,
    #[serde(default, skip_serializing_if = "is_normal")]
    pub blend: BlendMode,
    /// Photoshop's Fill: fades the content but not its effects.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub fill_opacity: f32,
    #[serde(default, skip_serializing_if = "LayerEffects::is_empty")]
    pub effects: LayerEffects,
}

impl Default for LayerProps {
    fn default() -> Self {
        LayerProps {
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            fill_opacity: 1.0,
            effects: LayerEffects::default(),
        }
    }
}

impl LayerProps {
    pub fn of(layer: &Layer) -> Self {
        LayerProps {
            visible: layer.visible,
            opacity: layer.opacity,
            blend: layer.blend,
            fill_opacity: layer.fill_opacity,
            effects: layer.effects.clone(),
        }
    }

    fn apply_to(&self, layer: &mut Layer) {
        layer.visible = self.visible;
        layer.opacity = self.opacity;
        layer.blend = self.blend;
        layer.fill_opacity = self.fill_opacity;
        layer.effects = self.effects.clone();
    }
}

/// One member of a clip group: a layer with content, or an adjustment that
/// changes the unit below it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClipMember {
    /// Ports: content, mask.
    Layer {
        #[serde(flatten)]
        props: Box<LayerProps>,
    },
    /// Port: mask (the content port stays empty).
    Adjustment {
        adjustment: Adjustment,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        visible: bool,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        opacity: f32,
        #[serde(default, skip_serializing_if = "is_normal")]
        blend: BlendMode,
    },
}

/// An operation and its parameters. In JSON the variant is the node's
/// `"type"` and the fields sit beside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Op {
    /// Transparent everywhere. The bottom of every stack.
    Empty,
    /// Pixels from the blob store (an imported photo, a painted layer).
    Image { blob: BlobId },
    /// Mask coverage: the blob's alpha where it has tiles, `default`
    /// elsewhere (1 reveals, 0 hides). No blob: `default` everywhere.
    Mask {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob: Option<BlobId>,
        default: f32,
    },
    /// One layer composited onto the backdrop: blend mode, opacity, fill,
    /// layer effects and mask.
    /// Ports: backdrop, content, mask.
    Layer {
        #[serde(flatten)]
        props: LayerProps,
    },
    /// An adjustment layer: changes the backdrop in place.
    /// Ports: backdrop, mask.
    Adjustment {
        adjustment: Adjustment,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        visible: bool,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        opacity: f32,
        #[serde(default, skip_serializing_if = "is_normal")]
        blend: BlendMode,
    },
    /// A live filter layer: the backdrop filtered, mixed in by opacity × mask.
    /// Ports: backdrop, mask.
    FilterLayer {
        filter: Filter,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        visible: bool,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        opacity: f32,
    },
    /// A pass-through group at partial strength: `after` (the group's
    /// layers composited straight onto `before`) mixed over `before`.
    /// Ports: before, after, mask.
    PassThrough {
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        visible: bool,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        opacity: f32,
    },
    /// A clipping group: the base layer and the layers clipped to it,
    /// composited as one unit with the base's blend and opacity.
    /// Ports: backdrop, base content, base mask, then content and mask for
    /// each member in order.
    ClipGroup {
        base: LayerProps,
        members: Vec<ClipMember>,
    },
}

impl Op {
    /// The name used as `"type"` in JSON.
    pub fn type_name(&self) -> &'static str {
        match self {
            Op::Empty => "empty",
            Op::Image { .. } => "image",
            Op::Mask { .. } => "mask",
            Op::Layer { .. } => "layer",
            Op::Adjustment { .. } => "adjustment",
            Op::FilterLayer { .. } => "filter-layer",
            Op::PassThrough { .. } => "pass-through",
            Op::ClipGroup { .. } => "clip-group",
        }
    }

    /// Names of the input ports, in order.
    pub fn ports(&self) -> &'static [&'static str] {
        match self {
            Op::Empty | Op::Image { .. } | Op::Mask { .. } => &[],
            Op::Layer { .. } => &["backdrop", "content", "mask"],
            Op::Adjustment { .. } | Op::FilterLayer { .. } => &["backdrop", "mask"],
            Op::PassThrough { .. } => &["before", "after", "mask"],
            Op::ClipGroup { .. } => &["backdrop", "base", "base-mask"],
        }
    }

    /// Ops whose port list continues past [`Op::ports`] (clip groups take a
    /// content and a mask port per member).
    pub fn variadic(&self) -> bool {
        matches!(self, Op::ClipGroup { .. })
    }
}

/// One shared tile of a constant colour, per value, for masks.
fn constant_tile(v: f32) -> Arc<Tile> {
    static ONES: OnceLock<Arc<Tile>> = OnceLock::new();
    if v == 1.0 {
        return ONES
            .get_or_init(|| Arc::new(Tile::filled(Rgba::new(1.0, 1.0, 1.0, 1.0))))
            .clone();
    }
    Arc::new(Tile::filled(Rgba::new(v, v, v, v)))
}

/// The tile-aligned area `pad` pixels around `coord`.
fn padded(coord: TileCoord, pad: i32) -> Rect {
    let (ox, oy) = coord.origin();
    Rect::new(
        ox - pad,
        oy - pad,
        TILE_SIZE as u32 + 2 * pad.max(0) as u32,
        TILE_SIZE as u32 + 2 * pad.max(0) as u32,
    )
}

/// A mask for the reference compositor from a mask input over `area`. Mask
/// ops return explicit tiles wherever coverage isn't zero, so the default
/// outside them is 0.
fn mask_over(ctx: &Ctx, node: Option<crate::NodeId>, area: Rect) -> Option<Mask> {
    let id = node?;
    Some(Mask {
        tiles: ctx.area(id, area),
        default: 0.0,
        enabled: true,
    })
}

/// A throwaway layer for the reference compositor.
fn temp_layer(content: LayerContent, props: &LayerProps, mask: Option<Mask>) -> Layer {
    let mut l = Layer::with_content(0, "", content);
    props.apply_to(&mut l);
    l.mask = mask;
    l
}

fn owned(t: Option<Arc<Tile>>) -> Option<Tile> {
    t.map(Arc::unwrap_or_clone)
}

fn grow(r: Rect, pad: i32) -> Rect {
    Rect::new(
        r.x - pad,
        r.y - pad,
        r.w + 2 * pad.max(0) as u32,
        r.h + 2 * pad.max(0) as u32,
    )
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(&b)),
        (a, b) => a.or(b),
    }
}

fn fx_pad(fx: &LayerEffects) -> i32 {
    if fx.is_empty() {
        0
    } else {
        fx.pad()
    }
}

/// A conservative bound on where `node`'s output can be non-transparent:
/// every tile outside it is empty. `None` means empty everywhere.
pub(crate) fn extent(ctx: &Ctx, id: crate::NodeId) -> Option<Rect> {
    let node = ctx.graph.node(id)?;
    let input = |i: usize| node.input(i).and_then(|n| ctx.extent(n));
    match &node.op {
        Op::Empty => None,
        Op::Image { blob } => ctx.blob(blob)?.bounds(),
        Op::Mask { blob, default } => {
            let tiles = blob.as_ref().and_then(|b| ctx.blob(b)).and_then(|s| s.bounds());
            if *default > 0.0 {
                union(tiles, Some(ctx.canvas))
            } else {
                tiles
            }
        }
        Op::Layer { props } => union(input(0), input(1).map(|r| grow(r, fx_pad(&props.effects)))),
        Op::Adjustment { .. } => input(0),
        Op::FilterLayer { filter, .. } => input(0).map(|r| grow(r, filter.pad())),
        Op::PassThrough { .. } => union(input(0), input(1)),
        Op::ClipGroup { base, members } => {
            let pad = std::iter::once(&base.effects)
                .chain(members.iter().filter_map(|m| match m {
                    ClipMember::Layer { props } => Some(&props.effects),
                    ClipMember::Adjustment { .. } => None,
                }))
                .map(fx_pad)
                .max()
                .unwrap_or(0);
            union(input(0), input(1).map(|r| grow(r, pad)))
        }
    }
}

/// The area a layer's content must be read over to render `coord`: the
/// tile plus the effects' reach, or all of the content when an effect is
/// laid out over the content's bounds (a gradient overlay).
fn content_area(ctx: &Ctx, content: crate::NodeId, fx: &LayerEffects, coord: TileCoord) -> Rect {
    let near = padded(coord, fx_pad(fx));
    if fx.gradient_overlay.is_some() {
        if let Some(all) = ctx.extent(content) {
            return near.union(&all);
        }
    }
    near
}

/// Evaluate one output tile of `node`. `None` means fully transparent.
pub(crate) fn eval_tile(ctx: &Ctx, node: &Node, coord: TileCoord) -> Option<Arc<Tile>> {
    let input = |i: usize| node.input(i);
    match &node.op {
        Op::Empty => None,
        Op::Image { blob } => ctx.blob(blob)?.tile_arc(coord).cloned(),
        Op::Mask { blob, default } => {
            let from_blob = blob
                .as_ref()
                .and_then(|b| ctx.blob(b))
                .and_then(|s| s.tile_arc(coord).cloned());
            match from_blob {
                Some(t) => Some(t),
                None if *default > 0.0 => Some(constant_tile(default.min(1.0))),
                None => None,
            }
        }
        Op::Layer { props } => {
            let backdrop = ctx.tile(input(0), coord);
            if !props.visible || props.opacity <= 0.0 {
                return backdrop;
            }
            let content = input(1)?;
            let area = content_area(ctx, content, &props.effects, coord);
            let store = ctx.area(content, area);
            if store.is_empty() {
                return backdrop;
            }
            let mask = mask_over(ctx, input(2), padded(coord, fx_pad(&props.effects)));
            if let Some(m) = &mask {
                if m.tiles.tile(coord).is_none() && props.effects.is_empty() {
                    return backdrop;
                }
            }
            let layer = temp_layer(LayerContent::Pixel(store), props, mask);
            lumenply_render::render_tile_over(owned(backdrop), &[layer], coord, ctx.canvas).map(Arc::new)
        }
        Op::Adjustment {
            adjustment,
            visible,
            opacity,
            blend,
        } => {
            let backdrop = ctx.tile(input(0), coord);
            if !*visible || *opacity <= 0.0 {
                return backdrop;
            }
            let mut tile = owned(backdrop)?;
            let mask = mask_over(ctx, input(1), coord.rect());
            if let Some(m) = &mask {
                if m.tiles.tile(coord).is_none() {
                    return Some(Arc::new(tile));
                }
            }
            lumenply_render::adjust_in_place(&mut tile, adjustment, *blend, *opacity, mask.as_ref(), coord);
            Some(Arc::new(tile))
        }
        Op::FilterLayer {
            filter,
            visible,
            opacity,
        } => {
            let backdrop_id = input(0);
            let backdrop = ctx.tile(backdrop_id, coord);
            if !*visible || *opacity <= 0.0 {
                return backdrop;
            }
            let mask = mask_over(ctx, input(1), coord.rect());
            let props = LayerProps {
                opacity: *opacity,
                ..LayerProps::default()
            };
            let layer = temp_layer(LayerContent::Filter(filter.clone()), &props, mask);
            let mut dst = owned(backdrop);
            lumenply_render::live_filter_into(
                &mut dst,
                &layer,
                coord,
                ctx.canvas,
                |area| match backdrop_id {
                    Some(b) => ctx.area(b, area),
                    None => TileStore::new(),
                },
            );
            dst.map(Arc::new)
        }
        Op::PassThrough { visible, opacity } => {
            let before = ctx.tile(input(0), coord);
            if !*visible || *opacity <= 0.0 {
                return before;
            }
            let after = ctx.tile(input(1), coord);
            let mask = mask_over(ctx, input(2), coord.rect());
            if mask.is_none() && *opacity >= 1.0 {
                return after;
            }
            lumenply_render::mix_tiles(owned(before), owned(after), *opacity, mask.as_ref(), coord)
                .map(Arc::new)
        }
        Op::ClipGroup { base, members } => {
            let backdrop = ctx.tile(input(0), coord);
            if !base.visible || base.opacity <= 0.0 {
                return backdrop;
            }
            // Effects anywhere in the unit read around the tile.
            let pad = std::iter::once(&base.effects)
                .chain(members.iter().filter_map(|m| match m {
                    ClipMember::Layer { props } => Some(&props.effects),
                    ClipMember::Adjustment { .. } => None,
                }))
                .map(fx_pad)
                .max()
                .unwrap_or(0);
            let area = padded(coord, pad);
            let base_store = match input(1) {
                Some(b) => ctx.area(b, content_area(ctx, b, &base.effects, coord).union(&area)),
                None => return backdrop,
            };
            let mut layers = vec![temp_layer(
                LayerContent::Pixel(base_store),
                base,
                mask_over(ctx, input(2), area),
            )];
            for (i, m) in members.iter().enumerate() {
                let (content_port, mask_port) = (3 + 2 * i, 4 + 2 * i);
                let mask = mask_over(ctx, input(mask_port), area);
                let mut l = match m {
                    ClipMember::Layer { props } => {
                        let store = input(content_port)
                            .map(|c| ctx.area(c, content_area(ctx, c, &props.effects, coord).union(&area)))
                            .unwrap_or_default();
                        temp_layer(LayerContent::Pixel(store), props, mask)
                    }
                    ClipMember::Adjustment {
                        adjustment,
                        visible,
                        opacity,
                        blend,
                    } => {
                        let props = LayerProps {
                            visible: *visible,
                            opacity: *opacity,
                            blend: *blend,
                            ..LayerProps::default()
                        };
                        temp_layer(LayerContent::Adjustment(adjustment.clone()), &props, mask)
                    }
                };
                l.clip = true;
                layers.push(l);
            }
            lumenply_render::render_tile_over(owned(backdrop), &layers, coord, ctx.canvas).map(Arc::new)
        }
    }
}
