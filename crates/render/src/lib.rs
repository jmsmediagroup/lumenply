//! Tiled compositor.
//!
//! Every tile of the output is independent, so tiles are rendered in
//! parallel with rayon. Blend math follows the W3C Compositing and Blending
//! spec on premultiplied, linear colour:
//!
//! ```text
//! co = cs·(1 − αb) + cb·(1 − αs) + αs·αb·B(Cb, Cs)
//! αo = αs + αb − αs·αb
//! ```
//!
//! where `Cb`, `Cs` are the straight (unpremultiplied) colours. The same
//! formulas will be ported to WGSL for the GPU path; this CPU path stays as
//! the reference and fallback.

pub mod cache;
pub mod develop;
pub mod fill;
pub mod filters;
mod filters_more;
pub mod gpu;
pub mod gradient_draw;
pub mod inpaint;
pub mod liquify;
pub mod membrane;
pub mod resample;
pub mod shape;
pub mod smart_filters;
pub mod text;
pub mod text_layout;
pub mod transform;

pub use cache::BelowCache;
pub use filters::{apply_filter, apply_filter_in_canvas, filter_raster, Filter};
pub use gpu::GpuCompositor;
pub use liquify::{liquify_preview, liquify_store, Displacement, LiquifyTool};
pub use transform::{
    perspective_mask, perspective_store, perspective_store_h, sample_bilinear, transform_mask,
    transform_store, warp_mask, warp_store, Homography, WarpGrid,
};

use lumenply_doc::{Adjustment, BlendMode, Document, Layer, LayerContent, Mask};
use lumenply_tiles::{Raster, Rect, Rgba, Tile, TileCoord, TileStore, TILE_PIXELS, TILE_SIZE};
use rayon::prelude::*;

/// Composite the whole canvas.
pub fn composite(doc: &Document) -> TileStore {
    composite_rect(doc, doc.canvas())
}

/// Composite only the tiles overlapping `rect`.
pub fn composite_rect(doc: &Document, rect: Rect) -> TileStore {
    composite_layers(doc.layers(), rect, doc.canvas())
}

/// Composite an arbitrary layer list (bottom-to-top) over `rect`. Used for
/// group previews and thumbnails as well as the document itself. `canvas`
/// tells live filters where the image ends (they clamp to that edge).
pub fn composite_layers(layers: &[Layer], rect: Rect, canvas: Rect) -> TileStore {
    let coords = rect.tiles();
    let filters: Vec<usize> = (0..layers.len())
        .filter(|&i| is_live_filter(&layers[i]))
        .collect();
    let tiles: Vec<(TileCoord, Option<Tile>)> = if filters.is_empty() {
        coords
            .into_par_iter()
            .map(|c| (c, render_tile(layers, c, canvas)))
            .collect()
    } else {
        composite_staged(layers, &filters, &coords, canvas)
    };

    let mut out = TileStore::new();
    for (c, tile) in tiles {
        if let Some(t) = tile {
            out.insert(c, std::sync::Arc::new(t));
        }
    }
    out
}

fn is_live_filter(l: &Layer) -> bool {
    l.visible && l.opacity > 0.0 && matches!(l.content, LayerContent::Filter(_))
}

/// The tile-aligned rectangle covering `coords`.
fn tiles_bounds(coords: &[TileCoord]) -> Rect {
    coords
        .iter()
        .map(|c| c.rect())
        .reduce(|a, b| a.union(&b))
        .unwrap_or(Rect::new(0, 0, 0, 0))
}

/// [`composite_layers`] for a stack with live filter layers (indices
/// `filters`, bottom first). Tile by tile, each filter would re-render
/// everything below it over its padded neighbourhood, and every filter
/// below that again: the cost doubles (or worse) per stacked filter. Here
/// the stack renders in stages split at the filters: each stage is
/// composited once over the tiles the stages above need, so every layer
/// renders once per tile. The per-tile maths is unchanged, so the result is
/// identical to [`render_tile`].
fn composite_staged(
    layers: &[Layer],
    filters: &[usize],
    out: &[TileCoord],
    canvas: Rect,
) -> Vec<(TileCoord, Option<Tile>)> {
    use std::collections::{HashMap, HashSet};
    let pad_of = |i: usize| match &layers[i].content {
        LayerContent::Filter(f) => f.pad(),
        _ => 0,
    };
    // From the top down: the stage below filter k (layers[..filters[k]])
    // must cover the tiles the stage above needs plus everything filter k
    // reads around them.
    let mut need: Vec<Vec<TileCoord>> = vec![Vec::new(); filters.len()];
    let mut above: Vec<TileCoord> = out.to_vec();
    for k in (0..filters.len()).rev() {
        let b = tiles_bounds(&above);
        let pad = pad_of(filters[k]);
        let grown =
            Rect::new(b.x - pad, b.y - pad, b.w + 2 * pad as u32, b.h + 2 * pad as u32).intersect(&canvas);
        let mut set: HashSet<TileCoord> = above.iter().copied().collect();
        set.extend(grown.tiles());
        need[k] = set.into_iter().collect();
        above = need[k].clone();
    }
    let below = &layers[..filters[0]];
    let mut stage: HashMap<TileCoord, Option<std::sync::Arc<Tile>>> = need[0]
        .par_iter()
        .map(|&c| (c, render_tile(below, c, canvas).map(std::sync::Arc::new)))
        .collect();
    for (k, &fi) in filters.iter().enumerate() {
        let end = filters.get(k + 1).copied().unwrap_or(layers.len());
        let targets: &[TileCoord] = if k + 1 < filters.len() { &need[k + 1] } else { out };
        let next: Vec<(TileCoord, Option<Tile>)> = targets
            .par_iter()
            .map(|&c| {
                let mut dst = stage.get(&c).and_then(|t| t.as_deref().cloned());
                live_filter_into(&mut dst, &layers[fi], c, canvas, |visible| {
                    let mut store = TileStore::new();
                    for t in visible.tiles() {
                        if let Some(Some(tile)) = stage.get(&t) {
                            store.insert(t, tile.clone());
                        }
                    }
                    store
                });
                (c, render_tile_over(dst, &layers[fi + 1..end], c, canvas))
            })
            .collect();
        if k + 1 == filters.len() {
            return next;
        }
        stage = next
            .into_iter()
            .map(|(c, t)| (c, t.map(std::sync::Arc::new)))
            .collect();
    }
    unreachable!("the loop returns at the last filter")
}

/// Composites live filter layer `layer` onto `dst`, tile `coord` of the
/// composite below it. A live filter needs the backdrop around the tile
/// too: `below(area)` returns the layers below composited over `area` (the
/// tile padded by the filter's reach, clipped to the canvas). Outside the
/// canvas the edge pixel is repeated, as Photoshop does, so borders don't
/// fade.
fn live_filter_into(
    dst: &mut Option<Tile>,
    layer: &Layer,
    coord: TileCoord,
    canvas: Rect,
    below: impl FnOnce(Rect) -> TileStore,
) {
    let LayerContent::Filter(f) = &layer.content else {
        return;
    };
    let mask = layer.mask.as_ref().filter(|m| m.enabled);
    if let Some(m) = mask {
        if m.default <= 0.0 && m.tiles.tile(coord).is_none() {
            return;
        }
    }
    let pad = f.pad();
    let (ox, oy) = coord.origin();
    let area = Rect::new(
        ox - pad,
        oy - pad,
        TILE_SIZE as u32 + 2 * pad as u32,
        TILE_SIZE as u32 + 2 * pad as u32,
    );
    let visible = area.intersect(&canvas);
    if visible.is_empty() {
        return;
    }
    let below = below(visible);
    if below.is_empty() && dst.is_none() {
        return;
    }
    let mut src = Raster::new(area.w, area.h);
    for y in 0..area.h as i32 {
        let sy = (area.y + y).clamp(canvas.y, canvas.bottom() - 1);
        for x in 0..area.w as i32 {
            let sx = (area.x + x).clamp(canvas.x, canvas.right() - 1);
            src.set(x as u32, y as u32, below.get_pixel(sx, sy));
        }
    }
    let filtered = filter_raster(&src, f, (area.x, area.y));
    let d = dst.get_or_insert_with(Tile::new);
    let mask_px = mask.and_then(|m| m.tiles.tile(coord)).map(|t| t.pixels());
    let mask_default = mask.map_or(1.0, |m| m.default);
    for (i, p) in d.pixels_mut().iter_mut().enumerate() {
        let w = layer.opacity * mask_px.as_ref().map_or(mask_default, |m| m[i].a);
        if w <= 0.0 {
            continue;
        }
        let (x, y) = (
            (i % TILE_SIZE) as u32 + pad as u32,
            (i / TILE_SIZE) as u32 + pad as u32,
        );
        let fp = filtered.get(x, y);
        *p = Rgba::new(
            p.r + (fp.r - p.r) * w,
            p.g + (fp.g - p.g) * w,
            p.b + (fp.b - p.b) * w,
            p.a + (fp.a - p.a) * w,
        );
    }
}

/// Composite the whole canvas into a dense raster.
pub fn composite_raster(doc: &Document) -> Raster {
    composite(doc).to_raster(doc.canvas())
}

/// Render one tile of a layer stack. Returns `None` when nothing touches it.
pub fn render_tile(layers: &[Layer], coord: TileCoord, canvas: Rect) -> Option<Tile> {
    render_tile_over(None, layers, coord, canvas)
}

/// Render one tile of a layer stack over an existing backdrop.
///
/// Note: a live [`LayerContent::Filter`] layer in `layers` reads its
/// backdrop from the slice alone and would ignore `backdrop`, so callers
/// must not pass one when the slice contains a visible filter layer
/// (see [`cache::BelowCache`], which falls back to the full path then).
pub fn render_tile_over(
    backdrop: Option<Tile>,
    layers: &[Layer],
    coord: TileCoord,
    canvas: Rect,
) -> Option<Tile> {
    let mut dst: Option<Tile> = backdrop;
    let mut skip_until = 0usize;
    for (idx, layer) in layers.iter().enumerate() {
        if idx < skip_until {
            continue;
        }
        // A clip chain: this layer is the base, the following `clip`
        // layers paint only inside its coverage, and the whole unit
        // composites with the base's blend and opacity.
        let chain_end = {
            let mut e = idx + 1;
            while e < layers.len() && layers[e].clip && !matches!(layers[e].content, LayerContent::Filter(_))
            {
                e += 1;
            }
            e
        };
        let baseable = !layer.clip
            && matches!(
                layer.content,
                LayerContent::Pixel(_)
                    | LayerContent::Text(_)
                    | LayerContent::Smart(_)
                    | LayerContent::Fill(_)
                    | LayerContent::Shape(_)
                    | LayerContent::Group(_)
            );
        if chain_end > idx + 1 && baseable {
            skip_until = chain_end;
            // A hidden base hides its whole chain.
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            let mask = layer.mask.as_ref().filter(|m| m.enabled);
            let Some(base_src) = source_tile(layer, coord, canvas) else {
                continue;
            };
            let mut unit = match mask {
                Some(m) => apply_mask(base_src, m, coord),
                None => base_src,
            };
            let base_alpha: Vec<f32> = unit.pixels().iter().map(|p| p.a).collect();
            for member in &layers[idx + 1..chain_end] {
                if !member.visible || member.opacity <= 0.0 {
                    continue;
                }
                let mmask = member.mask.as_ref().filter(|m| m.enabled);
                match &member.content {
                    LayerContent::Adjustment(adj) => {
                        // adjust_in_place skips zero-alpha pixels, so the
                        // base's coverage gates it automatically.
                        adjust_in_place(&mut unit, adj, member.blend, member.opacity, mmask, coord);
                    }
                    _ => {
                        let Some(src) = source_tile(member, coord, canvas) else {
                            continue;
                        };
                        let masked = match mmask {
                            Some(m) => apply_mask(src, m, coord),
                            None => src,
                        };
                        if member.effects.is_empty() {
                            blend_tile(&mut unit, &masked, member.blend, member.opacity);
                        } else {
                            // A member's effects render inside the unit; the
                            // alpha force-back below clips them to the base.
                            render_effects_under(&mut unit, member, coord, canvas);
                            blend_tile(&mut unit, &masked, member.blend, member.opacity);
                            render_overlays_over(&mut unit, member, coord, canvas);
                            render_bevel_over(&mut unit, member, coord, canvas);
                            render_inner_over(&mut unit, member, coord, canvas);
                            render_stroke_over(&mut unit, member, coord, canvas);
                        }
                    }
                }
            }
            // Clipped content never changes coverage: restore the base's
            // alpha, keeping the blended straight colour.
            for (i, p) in unit.pixels_mut().iter_mut().enumerate() {
                let ba = base_alpha[i];
                if ba <= 0.0 || p.a <= 0.0 {
                    *p = Rgba::TRANSPARENT;
                } else if (p.a - ba).abs() > 1e-7 {
                    let k = ba / p.a;
                    *p = Rgba::new(p.r * k, p.g * k, p.b * k, ba);
                }
            }
            let d = dst.get_or_insert_with(Tile::new);
            if layer.effects.is_empty() {
                blend_tile(d, &unit, layer.blend, layer.opacity);
            } else {
                // The base's own effects come from the base's coverage:
                // shadow and glow under the whole unit, the rest above it
                // (so an opaque overlay covers clipped content, as in
                // Photoshop).
                render_effects_under(d, layer, coord, canvas);
                blend_tile(d, &unit, layer.blend, layer.opacity);
                render_overlays_over(d, layer, coord, canvas);
                render_bevel_over(d, layer, coord, canvas);
                render_inner_over(d, layer, coord, canvas);
                render_stroke_over(d, layer, coord, canvas);
            }
            continue;
        }
        if !layer.visible || layer.opacity <= 0.0 {
            continue;
        }
        // A mask that hides this whole tile means the layer can be skipped.
        let mask = layer.mask.as_ref().filter(|m| m.enabled);
        if let Some(m) = mask {
            if m.default <= 0.0 && m.tiles.tile(coord).is_none() {
                continue;
            }
        }

        let owned: Tile;
        let src: &Tile = match &layer.content {
            LayerContent::Pixel(_)
            | LayerContent::Text(_)
            | LayerContent::Smart(_)
            | LayerContent::Fill(_)
            | LayerContent::Shape(_) => {
                let Some(store) = layer.raster_store() else {
                    continue;
                };
                match store.tile(coord) {
                    Some(t) if mask.is_none() => t,
                    Some(t) => {
                        owned = t.clone();
                        &owned
                    }
                    None => continue,
                }
            }
            LayerContent::Group(children) => {
                // Pass-through: the children composite straight onto the
                // backdrop, so adjustments and blend modes inside the group
                // reach the layers below it. A live filter child still
                // needs the isolated path (it reads its backdrop from the
                // child slice alone, see render_tile_over).
                let has_filter = children
                    .iter()
                    .any(|c| c.visible && c.opacity > 0.0 && matches!(c.content, LayerContent::Filter(_)));
                // Effects need a defined group raster, so a styled group
                // composites isolated even when set to pass-through (as
                // Photoshop does).
                if layer.pass_through && !has_filter && layer.effects.is_empty() {
                    let before = dst.clone();
                    let after = render_tile_over(before.clone(), children, coord, canvas);
                    dst = mix_tiles(before, after, layer.opacity, mask, coord);
                    continue;
                }
                match render_tile(children, coord, canvas) {
                    Some(t) => {
                        owned = t;
                        &owned
                    }
                    None => continue,
                }
            }
            LayerContent::Adjustment(adj) => {
                // Adjustments modify the backdrop in place rather than
                // compositing over it, so alpha is never accumulated.
                if let Some(below) = dst.as_mut() {
                    adjust_in_place(below, adj, layer.blend, layer.opacity, mask, coord);
                }
                continue;
            }
            LayerContent::Filter(_) => {
                live_filter_into(&mut dst, layer, coord, canvas, |area| {
                    composite_layers(&layers[..idx], area, canvas)
                });
                continue;
            }
        };

        let fx = &layer.effects;
        if !fx.is_empty() {
            // Effects render from the layer's own (masked) coverage over a
            // padded neighbourhood: shadow and glow under the layer,
            // stroke over it.
            let masked = match mask {
                Some(m) => apply_mask(src.clone(), m, coord),
                None => src.clone(),
            };
            let d = dst.get_or_insert_with(Tile::new);
            render_effects_under(d, layer, coord, canvas);
            blend_tile(d, &masked, layer.blend, layer.opacity);
            render_overlays_over(d, layer, coord, canvas);
            render_bevel_over(d, layer, coord, canvas);
            render_inner_over(d, layer, coord, canvas);
            render_stroke_over(d, layer, coord, canvas);
            continue;
        }
        let d = dst.get_or_insert_with(Tile::new);
        match mask {
            Some(m) => {
                let masked = apply_mask(src.clone(), m, coord);
                blend_tile(d, &masked, layer.blend, layer.opacity);
            }
            None => blend_tile(d, src, layer.blend, layer.opacity),
        }
    }
    dst
}

/// The layer's masked coverage over `area` (alpha only, 0..1).
fn coverage_raster(layer: &Layer, area: Rect, canvas: Rect) -> Vec<f32> {
    let (w, h) = (area.w as usize, area.h as usize);
    let mut out = vec![0f32; w * h];
    let mask = layer.mask.as_ref().filter(|m| m.enabled);
    match &layer.content {
        LayerContent::Pixel(_)
        | LayerContent::Text(_)
        | LayerContent::Smart(_)
        | LayerContent::Fill(_)
        | LayerContent::Shape(_) => {
            if let Some(store) = layer.raster_store() {
                for gy in 0..h {
                    for gx in 0..w {
                        let (x, y) = (area.x + gx as i32, area.y + gy as i32);
                        let mut a = store.get_pixel(x, y).a;
                        if let Some(m) = mask {
                            a *= m.value(x, y);
                        }
                        out[gy * w + gx] = a;
                    }
                }
            }
        }
        LayerContent::Group(children) => {
            let flat = composite_layers(children, area, canvas);
            for gy in 0..h {
                for gx in 0..w {
                    let (x, y) = (area.x + gx as i32, area.y + gy as i32);
                    let mut a = flat.get_pixel(x, y).a;
                    if let Some(m) = mask {
                        a *= m.value(x, y);
                    }
                    out[gy * w + gx] = a;
                }
            }
        }
        _ => {}
    }
    out
}

/// Gaussian-ish blur of a scalar field (three box passes, like the filters).
fn blur_field(src: &[f32], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let mut raster = Raster::new(w as u32, h as u32);
    for (i, &a) in src.iter().enumerate() {
        raster.pixels[i] = Rgba::new(0.0, 0.0, 0.0, a);
    }
    let blurred = filter_raster(&raster, &Filter::GaussianBlur { radius }, (0, 0));
    blurred.pixels.iter().map(|p| p.a).collect()
}

/// Shadow and glow, blended under the layer (onto `dst` before the layer).
fn render_effects_under(dst: &mut Tile, layer: &Layer, coord: TileCoord, canvas: Rect) {
    let fx = &layer.effects;
    if fx.drop_shadow.is_none() && fx.outer_glow.is_none() {
        return;
    }
    let pad = fx.pad();
    let (ox, oy) = coord.origin();
    let area = Rect::new(
        ox - pad,
        oy - pad,
        TILE_SIZE as u32 + 2 * pad as u32,
        TILE_SIZE as u32 + 2 * pad as u32,
    );
    let cov = coverage_raster(layer, area, canvas);
    let (w, h) = (area.w as usize, area.h as usize);
    let at = |field: &[f32], x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            0.0
        } else {
            field[y as usize * w + x as usize]
        }
    };
    struct FxPass {
        field: Vec<f32>,
        offset: (f32, f32),
        color: [f32; 3],
        opacity: f32,
    }
    let mut passes: Vec<FxPass> = Vec::new();
    if let Some(sfx) = &fx.drop_shadow {
        passes.push(FxPass {
            field: blur_field(&cov, w, h, sfx.blur),
            offset: (sfx.dx, sfx.dy),
            color: sfx.color,
            opacity: sfx.opacity,
        });
    }
    if let Some(g) = &fx.outer_glow {
        passes.push(FxPass {
            field: blur_field(&cov, w, h, g.blur),
            offset: (0.0, 0.0),
            color: g.color,
            opacity: g.opacity,
        });
    }
    for FxPass {
        field,
        offset: (dx, dy),
        color,
        opacity,
    } in passes
    {
        let mut tile = Tile::new();
        let px = tile.pixels_mut();
        for row in 0..TILE_SIZE {
            for col in 0..TILE_SIZE {
                let gx = col as i32 + pad - dx.round() as i32;
                let gy = row as i32 + pad - dy.round() as i32;
                let a = at(&field, gx, gy) * opacity;
                if a > 0.0 {
                    px[row * TILE_SIZE + col] =
                        Rgba::from_straight(color[0], color[1], color[2], a.clamp(0.0, 1.0));
                }
            }
        }
        blend_tile(dst, &tile, BlendMode::Normal, layer.opacity);
    }
}

/// Colour and gradient overlays, painted over the layer's coverage. The
/// gradient spans the layer's content bounds (the canvas for groups,
/// whose bounds would mean compositing twice).
fn render_overlays_over(dst: &mut Tile, layer: &Layer, coord: TileCoord, canvas: Rect) {
    let fx = &layer.effects;
    if fx.color_overlay.is_none() && fx.gradient_overlay.is_none() {
        return;
    }
    let (ox, oy) = coord.origin();
    let area = Rect::new(ox, oy, TILE_SIZE as u32, TILE_SIZE as u32);
    let cov = coverage_raster(layer, area, canvas);
    let w = area.w as usize;
    let paint = |dst: &mut Tile, color_at: &dyn Fn(i32, i32) -> ([f32; 3], f32), opacity: f32| {
        let mut tile = Tile::new();
        let px = tile.pixels_mut();
        for row in 0..TILE_SIZE {
            for col in 0..TILE_SIZE {
                let clip = cov[row * w + col];
                if clip <= 0.0 {
                    continue;
                }
                let (c, tint) = color_at(ox + col as i32, oy + row as i32);
                let a = (opacity * tint * clip).clamp(0.0, 1.0);
                if a > 0.0 {
                    px[row * TILE_SIZE + col] = Rgba::from_straight(c[0], c[1], c[2], a);
                }
            }
        }
        blend_tile(dst, &tile, BlendMode::Normal, layer.opacity);
    };
    if let Some(co) = &fx.color_overlay {
        paint(dst, &|_, _| (co.color, 1.0), co.opacity);
    }
    if let Some(go) = &fx.gradient_overlay {
        let bounds = match layer.raster_store().and_then(|s| s.content_bounds()) {
            Some(b) => b,
            None => canvas,
        };
        // Project the bounds' corners onto the gradient direction so t
        // spans exactly 0..1 across the content, whatever the angle.
        let rad = go.angle.to_radians();
        let (dx, dy) = (rad.cos(), -rad.sin()); // y grows downward
        let corners = [
            (bounds.x as f32, bounds.y as f32),
            (bounds.right() as f32, bounds.y as f32),
            (bounds.x as f32, bounds.bottom() as f32),
            (bounds.right() as f32, bounds.bottom() as f32),
        ];
        let dots: Vec<f32> = corners.iter().map(|(x, y)| x * dx + y * dy).collect();
        let lo = dots.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let hi = dots.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let span = (hi - lo).max(1e-3);
        paint(
            dst,
            &|x, y| {
                let t = (((x as f32 + 0.5) * dx + (y as f32 + 0.5) * dy) - lo) / span;
                let t = t.clamp(0.0, 1.0);
                let mix = |a: f32, b: f32| a + (b - a) * t;
                (
                    [
                        mix(go.start[0], go.end[0]),
                        mix(go.start[1], go.end[1]),
                        mix(go.start[2], go.end[2]),
                    ],
                    1.0,
                )
            },
            go.opacity,
        );
    }
}

/// Inner bevel: emboss lighting from the gradient of the blurred coverage.
/// The slope normal faces the light on one flank (highlight) and away on
/// the other (shadow); flat interiors get neither.
fn render_bevel_over(dst: &mut Tile, layer: &Layer, coord: TileCoord, canvas: Rect) {
    let Some(b) = &layer.effects.bevel else {
        return;
    };
    let size = lumenply_doc::sane_radius(b.size).max(0.5);
    let pad = layer.effects.pad();
    let (ox, oy) = coord.origin();
    let area = Rect::new(
        ox - pad,
        oy - pad,
        TILE_SIZE as u32 + 2 * pad as u32,
        TILE_SIZE as u32 + 2 * pad as u32,
    );
    let cov = coverage_raster(layer, area, canvas);
    let (w, h) = (area.w as usize, area.h as usize);
    let field = blur_field(&cov, w, h, size);
    let at = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            0.0
        } else {
            field[y as usize * w + x as usize]
        }
    };
    let rad = b.angle.to_radians();
    let (lx, ly) = (rad.cos(), -rad.sin()); // y grows downward
                                            // The slope of a blurred step is ~1/(2·size) per pixel; scale so a
                                            // full edge at depth 1 reaches full-strength lighting.
    let gain = 2.0 * size * b.depth;
    let mut tile = Tile::new();
    let px = tile.pixels_mut();
    for row in 0..TILE_SIZE {
        for col in 0..TILE_SIZE {
            let clip = cov[(row + pad as usize) * w + col + pad as usize];
            if clip <= 0.0 {
                continue;
            }
            let (gx, gy) = (col as i32 + pad, row as i32 + pad);
            // Central-difference gradient of the blurred coverage.
            let dx = (at(gx + 1, gy) - at(gx - 1, gy)) / 2.0;
            let dy = (at(gx, gy + 1) - at(gx, gy - 1)) / 2.0;
            // The surface rises where coverage falls off, so the outward
            // normal points along -grad; light from `angle` hits it when
            // the two align.
            let s = -(dx * lx + dy * ly) * gain;
            let a = (s.abs().min(1.0) * b.opacity * clip).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let c = if s > 0.0 { b.highlight } else { b.shadow };
            px[row * TILE_SIZE + col] = Rgba::from_straight(c[0], c[1], c[2], a);
        }
    }
    blend_tile(dst, &tile, BlendMode::Normal, layer.opacity);
}

/// Inner shadow and inner glow, blended over the layer and clipped to its
/// coverage: the blurred *inverse* coverage creeps in from the edge.
fn render_inner_over(dst: &mut Tile, layer: &Layer, coord: TileCoord, canvas: Rect) {
    let fx = &layer.effects;
    if fx.inner_shadow.is_none() && fx.inner_glow.is_none() {
        return;
    }
    let pad = fx.pad();
    let (ox, oy) = coord.origin();
    let area = Rect::new(
        ox - pad,
        oy - pad,
        TILE_SIZE as u32 + 2 * pad as u32,
        TILE_SIZE as u32 + 2 * pad as u32,
    );
    let cov = coverage_raster(layer, area, canvas);
    let (w, h) = (area.w as usize, area.h as usize);
    let inv: Vec<f32> = cov.iter().map(|&a| 1.0 - a).collect();
    let at = |field: &[f32], x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            // Outside the padded window counts as outside the layer.
            1.0
        } else {
            field[y as usize * w + x as usize]
        }
    };
    struct FxPass {
        field: Vec<f32>,
        offset: (f32, f32),
        color: [f32; 3],
        opacity: f32,
    }
    let mut passes: Vec<FxPass> = Vec::new();
    if let Some(s) = &fx.inner_shadow {
        passes.push(FxPass {
            field: blur_field(&inv, w, h, s.blur),
            offset: (s.dx, s.dy),
            color: s.color,
            opacity: s.opacity,
        });
    }
    if let Some(g) = &fx.inner_glow {
        passes.push(FxPass {
            field: blur_field(&inv, w, h, g.blur),
            offset: (0.0, 0.0),
            color: g.color,
            opacity: g.opacity,
        });
    }
    for FxPass {
        field,
        offset: (dx, dy),
        color,
        opacity,
    } in passes
    {
        let mut tile = Tile::new();
        let px = tile.pixels_mut();
        for row in 0..TILE_SIZE {
            for col in 0..TILE_SIZE {
                let clip = cov[(row as i32 + pad) as usize * w + (col as i32 + pad) as usize];
                if clip <= 0.0 {
                    continue;
                }
                let gx = col as i32 + pad - dx.round() as i32;
                let gy = row as i32 + pad - dy.round() as i32;
                let a = at(&field, gx, gy) * opacity * clip;
                if a > 0.0 {
                    px[row * TILE_SIZE + col] =
                        Rgba::from_straight(color[0], color[1], color[2], a.clamp(0.0, 1.0));
                }
            }
        }
        blend_tile(dst, &tile, BlendMode::Normal, layer.opacity);
    }
}

/// The outline stroke, blended over the layer: a ring grown outward from
/// the coverage edge by a chamfer distance transform.
fn render_stroke_over(dst: &mut Tile, layer: &Layer, coord: TileCoord, canvas: Rect) {
    let Some(stroke) = &layer.effects.stroke else {
        return;
    };
    let size = lumenply_doc::sane_radius(stroke.size);
    if size <= 0.0 {
        return;
    }
    let pad = layer.effects.pad();
    let (ox, oy) = coord.origin();
    let area = Rect::new(
        ox - pad,
        oy - pad,
        TILE_SIZE as u32 + 2 * pad as u32,
        TILE_SIZE as u32 + 2 * pad as u32,
    );
    let cov = coverage_raster(layer, area, canvas);
    let (w, h) = (area.w as usize, area.h as usize);
    // Two-pass 3-4 chamfer distance (in pixels / 3) to coverage >= 0.5.
    const BIG: f32 = 1e6;
    let mut d: Vec<f32> = cov.iter().map(|&a| if a >= 0.5 { 0.0 } else { BIG }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut best = d[i];
            if x > 0 {
                best = best.min(d[i - 1] + 3.0);
            }
            if y > 0 {
                best = best.min(d[i - w] + 3.0);
                if x > 0 {
                    best = best.min(d[i - w - 1] + 4.0);
                }
                if x + 1 < w {
                    best = best.min(d[i - w + 1] + 4.0);
                }
            }
            d[i] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut best = d[i];
            if x + 1 < w {
                best = best.min(d[i + 1] + 3.0);
            }
            if y + 1 < h {
                best = best.min(d[i + w] + 3.0);
                if x > 0 {
                    best = best.min(d[i + w - 1] + 4.0);
                }
                if x + 1 < w {
                    best = best.min(d[i + w + 1] + 4.0);
                }
            }
            d[i] = best;
        }
    }
    let mut tile = Tile::new();
    let px = tile.pixels_mut();
    for row in 0..TILE_SIZE {
        for col in 0..TILE_SIZE {
            let i = (row + pad as usize) * w + col + pad as usize;
            let dist = d[i] / 3.0;
            if dist <= 0.0 {
                continue; // inside the coverage: the stroke grows outward
            }
            let ring = (size + 0.5 - dist).clamp(0.0, 1.0) * stroke.opacity;
            if ring > 0.0 {
                px[row * TILE_SIZE + col] =
                    Rgba::from_straight(stroke.color[0], stroke.color[1], stroke.color[2], ring);
            }
        }
    }
    blend_tile(dst, &tile, BlendMode::Normal, layer.opacity);
}

/// A layer's own content for one tile — pixels, the text cache, or an
/// isolated render of a group — before masks and blending.
fn source_tile(layer: &Layer, coord: TileCoord, canvas: Rect) -> Option<Tile> {
    match &layer.content {
        LayerContent::Pixel(_)
        | LayerContent::Text(_)
        | LayerContent::Smart(_)
        | LayerContent::Fill(_)
        | LayerContent::Shape(_) => layer.raster_store()?.tile(coord).cloned(),
        LayerContent::Group(children) => render_tile(children, coord, canvas),
        _ => None,
    }
}

/// Mix `after` over `before` by `opacity × mask`: the result of a
/// pass-through group at partial strength. `None` means a transparent tile.
fn mix_tiles(
    before: Option<Tile>,
    after: Option<Tile>,
    opacity: f32,
    mask: Option<&Mask>,
    coord: TileCoord,
) -> Option<Tile> {
    if opacity >= 1.0 && mask.is_none() {
        return after;
    }
    let (mut out, b) = match (before, after) {
        (None, None) => return None,
        (b, a) => (a.unwrap_or_default(), b.unwrap_or_default()),
    };
    let mask_px = mask.and_then(|m| m.tiles.tile(coord)).map(|t| t.pixels());
    let mask_default = mask.map_or(1.0, |m| m.default);
    let bp = b.pixels();
    for (i, p) in out.pixels_mut().iter_mut().enumerate() {
        let w = opacity * mask_px.as_ref().map_or(mask_default, |m| m[i].a);
        let q = bp[i];
        *p = Rgba::new(
            q.r + (p.r - q.r) * w,
            q.g + (p.g - q.g) * w,
            q.b + (p.b - q.b) * w,
            q.a + (p.a - q.a) * w,
        );
    }
    Some(out)
}

/// Apply an adjustment layer to a backdrop tile in place. The adjusted
/// colour is mixed in by `opacity × mask`; with a non-normal blend mode the
/// adjusted colour is first blended against the original. Alpha is kept.
pub fn adjust_in_place(
    tile: &mut Tile,
    adj: &Adjustment,
    mode: BlendMode,
    opacity: f32,
    mask: Option<&Mask>,
    coord: TileCoord,
) {
    let mask_px = mask.and_then(|m| m.tiles.tile(coord)).map(|t| t.pixels());
    let mask_default = mask.map_or(1.0, |m| m.default);
    let adj = adj.compile();
    for (i, p) in tile.pixels_mut().iter_mut().enumerate() {
        if p.a <= 0.0 {
            continue;
        }
        let w = opacity * mask_px.as_ref().map_or(mask_default, |m| m[i].a);
        if w <= 0.0 {
            continue;
        }
        let [r, g, b, a] = p.to_straight();
        let [ar, ag, ab] = adj.apply([r, g, b]);
        let mix = |c: f32, ac: f32| c + (blend_channel(mode, c, ac) - c) * w;
        *p = Rgba::from_straight(mix(r, ar), mix(g, ag), mix(b, ab), a);
    }
}

/// Run an adjustment over every pixel of a tile, preserving alpha.
pub fn adjust_tile(tile: &Tile, adj: &Adjustment) -> Tile {
    let mut out = tile.clone();
    let adj = adj.compile();
    for p in out.pixels_mut() {
        if p.a <= 0.0 {
            continue;
        }
        let [r, g, b, a] = p.to_straight();
        let [r, g, b] = adj.apply([r, g, b]);
        *p = Rgba::from_straight(r, g, b, a);
    }
    out
}

/// Multiply a tile by a mask's coverage.
pub fn apply_mask(mut tile: Tile, mask: &Mask, coord: TileCoord) -> Tile {
    match mask.tiles.tile(coord) {
        Some(m) => {
            let mp = m.pixels();
            for (i, p) in tile.pixels_mut().iter_mut().enumerate() {
                *p = p.scale(mp[i].a);
            }
        }
        None => {
            if mask.default < 1.0 {
                for p in tile.pixels_mut() {
                    *p = p.scale(mask.default);
                }
            }
        }
    }
    tile
}

/// Paint a horizontal gradient into a mask across `rect` (0 at the left
/// edge, 1 at the right). Handy for tests and demos.
pub fn gradient_mask(mask: &mut Mask, rect: Rect) {
    for c in rect.tiles() {
        let sub = rect.intersect(&c.rect());
        for py in sub.y..sub.bottom() {
            for px in sub.x..sub.right() {
                let v = (px - rect.x) as f32 / (rect.w.max(2) - 1) as f32;
                mask.set_value(px, py, v);
            }
        }
    }
}

/// Blend `src` onto `dst` in place.
pub fn blend_tile(dst: &mut Tile, src: &Tile, mode: BlendMode, opacity: f32) {
    let d = dst.pixels_mut();
    let s = src.pixels();
    debug_assert_eq!(d.len(), TILE_PIXELS);
    for i in 0..TILE_PIXELS {
        d[i] = blend_pixel(d[i], s[i], mode, opacity);
    }
}

/// Blend one source pixel (scaled by `opacity`) onto a backdrop pixel.
#[inline]
pub fn blend_pixel(backdrop: Rgba, source: Rgba, mode: BlendMode, opacity: f32) -> Rgba {
    let s = if opacity >= 1.0 {
        source
    } else {
        source.scale(opacity)
    };
    if s.a <= 0.0 {
        return backdrop;
    }
    if mode == BlendMode::Normal || backdrop.a <= 0.0 {
        return s.over(backdrop);
    }
    let b = backdrop;
    let ab = b.a;
    let as_ = s.a;
    let cb = [b.r / ab, b.g / ab, b.b / ab];
    let cs = [s.r / as_, s.g / as_, s.b / as_];
    let both = as_ * ab;
    let mix = |c_s: f32, c_b: f32, i: usize| -> f32 {
        c_s * (1.0 - ab) + c_b * (1.0 - as_) + both * blend_channel(mode, cb[i], cs[i])
    };
    Rgba::new(
        mix(s.r, b.r, 0),
        mix(s.g, b.g, 1),
        mix(s.b, b.b, 2),
        as_ + ab - both,
    )
}

/// Separable blend function `B(Cb, Cs)` on straight colour in `[0, 1]`.
#[inline]
pub fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Add => (cb + cs).min(1.0),
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::SoftLight => soft_light(cb, cs),
    }
}

#[inline]
fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        let s = 2.0 * cs - 1.0;
        cb + s - cb * s
    }
}

#[inline]
fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::Layer;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn straight(p: Rgba) -> [f32; 4] {
        p.to_straight()
    }

    #[test]
    fn multiply_on_opaque_backdrop() {
        let b = Rgba::from_straight(0.5, 0.5, 0.5, 1.0);
        let s = Rgba::from_straight(0.5, 1.0, 0.0, 1.0);
        let o = straight(blend_pixel(b, s, BlendMode::Multiply, 1.0));
        assert!(close(o[0], 0.25) && close(o[1], 0.5) && close(o[2], 0.0) && close(o[3], 1.0));
    }

    #[test]
    fn opacity_scales_the_effect() {
        let b = Rgba::WHITE;
        let s = Rgba::BLACK;
        let o = straight(blend_pixel(b, s, BlendMode::Normal, 0.25));
        assert!(close(o[0], 0.75) && close(o[3], 1.0));
    }

    #[test]
    fn blending_onto_transparent_is_plain_over() {
        let s = Rgba::from_straight(0.2, 0.4, 0.6, 0.5);
        for mode in BlendMode::ALL {
            assert_eq!(blend_pixel(Rgba::TRANSPARENT, s, mode, 1.0), s);
        }
    }

    #[test]
    fn screen_and_difference_identities() {
        let b = Rgba::from_straight(0.3, 0.6, 0.9, 1.0);
        let screen = straight(blend_pixel(b, Rgba::BLACK, BlendMode::Screen, 1.0));
        assert!(close(screen[0], 0.3) && close(screen[2], 0.9));
        let diff = straight(blend_pixel(b, b, BlendMode::Difference, 1.0));
        assert!(close(diff[0], 0.0) && close(diff[1], 0.0) && close(diff[2], 0.0));
    }

    #[test]
    fn groups_composite_as_isolated_units() {
        let mut doc = Document::new(4, 4);
        let bg = doc.add_pixel_layer("bg");
        for y in 0..4 {
            for x in 0..4 {
                doc.layer_mut(bg)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::WHITE);
            }
        }
        // A group containing a half-opaque black layer, with the group itself at 50%.
        let gid = doc.add_group("g");
        let inner_id = doc.alloc_id();
        let mut inner = Layer::pixel(inner_id, "inner");
        inner
            .pixels_mut()
            .unwrap()
            .set_pixel(1, 1, Rgba::from_straight(0.0, 0.0, 0.0, 1.0));
        inner.opacity = 0.5;
        let g = doc.layer_mut(gid).unwrap();
        g.opacity = 0.5;
        g.children_mut().unwrap().push(inner);

        let out = composite_raster(&doc);
        let p = straight(out.get(1, 1));
        assert!(close(p[0], 0.75), "got {p:?}");
        let untouched = straight(out.get(0, 0));
        assert!(close(untouched[0], 1.0));
    }

    #[test]
    fn layer_effects_render_shadow_glow_and_stroke() {
        use lumenply_doc::{LayerEffects, ShadowFx, StrokeFx};
        let mut doc = Document::new(96, 96);
        let id = doc.add_pixel_layer("square");
        for y in 40..56 {
            for x in 40..56 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.6, 0.9, 1.0),
                );
            }
        }
        doc.layer_mut(id).unwrap().effects = LayerEffects {
            drop_shadow: Some(ShadowFx {
                dx: 8.0,
                dy: 8.0,
                blur: 2.0,
                color: [0.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            stroke: Some(StrokeFx {
                size: 3.0,
                color: [1.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);

        // Shadow: below-right of the square, dark and present.
        let sh = out.get(60, 60);
        assert!(sh.a > 0.5, "shadow coverage at the offset: {sh:?}");
        let shc = straight(sh);
        assert!(shc[0] < 0.1, "shadow is dark: {shc:?}");
        // No shadow far away.
        assert!(out.get(20, 20).a < 1e-3);

        // The layer itself renders over its own shadow.
        let body = straight(out.get(48, 48));
        assert!(close(body[2], 0.9), "body colour intact: {body:?}");

        // Stroke: a red ring just outside the square, not inside.
        let ring = straight(out.get(38, 48));
        assert!(
            ring[0] > 0.9 && ring[1] < 0.1,
            "stroke outside the edge: {ring:?}"
        );
        let inside = straight(out.get(44, 48));
        assert!(close(inside[2], 0.9), "no stroke inside: {inside:?}");
        // The ring ends ~3px out.
        assert!(out.get(35, 48).a < 0.6, "ring width bounded");

        // Tile-seam safety: the same document shifted across the 256-tile
        // boundary renders effects identically (relative to the shift).
        let mut doc2 = Document::new(400, 96);
        let id2 = doc2.add_pixel_layer("square");
        for y in 40..56 {
            for x in 248..264 {
                doc2.layer_mut(id2).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.6, 0.9, 1.0),
                );
            }
        }
        doc2.layer_mut(id2).unwrap().effects = doc.layer(id).unwrap().effects.clone();
        let out2 = composite_raster(&doc2);
        for dy in 0..96 {
            for dx in 0..60 {
                let a = out.get(30 + dx, dy).a;
                let b = out2.get(238 + dx, dy).a;
                assert!((a - b).abs() < 2e-3, "seam mismatch at {dx},{dy}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn effects_work_on_clip_members_and_isolate_styled_pass_through_groups() {
        use lumenply_doc::{LayerEffects, ShadowFx, StrokeFx};
        // Base: grey square 20..40 in both axes. Clipped member: red square
        // x 32..44, y 24..32 (crossing the base's right edge) with a green
        // stroke and a drop shadow.
        let mut doc = Document::new(64, 64);
        let base = doc.add_pixel_layer("base");
        for y in 20..40 {
            for x in 20..40 {
                doc.layer_mut(base).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.5, 0.5, 0.5, 1.0),
                );
            }
        }
        let member = doc.add_pixel_layer("clipped");
        for y in 24..32 {
            for x in 32..44 {
                doc.layer_mut(member).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.9, 0.1, 0.1, 1.0),
                );
            }
        }
        doc.layer_mut(member).unwrap().clip = true;
        doc.layer_mut(member).unwrap().effects = LayerEffects {
            stroke: Some(StrokeFx {
                size: 2.0,
                color: [0.0, 1.0, 0.0],
                opacity: 1.0,
            }),
            drop_shadow: Some(ShadowFx {
                dx: 0.0,
                dy: 4.0,
                blur: 1.0,
                color: [0.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        let ring = straight(out.get(31, 28)); // stroke left of the member, inside the base
        assert!(
            ring[1] > 0.8 && ring[0] < 0.2,
            "member stroke inside the base: {ring:?}"
        );
        assert!(
            out.get(45, 28).a < 1e-4,
            "stroke outside the base is clipped away"
        );
        let shadow = straight(out.get(36, 34)); // below the member, inside the base
        assert!(
            shadow[0] < 0.15,
            "member shadow darkens the base fill: {shadow:?}"
        );
        assert!(
            out.get(36, 42).a < 1e-4,
            "shadow outside the base is clipped away"
        );
        let red = straight(out.get(36, 28));
        assert!(red[0] > 0.8 && red[1] < 0.2, "member paint shows inside: {red:?}");

        // The base's own stroke still rings the base, outside it.
        doc.layer_mut(base).unwrap().effects = LayerEffects {
            stroke: Some(StrokeFx {
                size: 2.0,
                color: [0.0, 0.2, 1.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        let base_ring = straight(out.get(18, 30));
        assert!(base_ring[2] > 0.8, "base stroke outside the base: {base_ring:?}");

        // A styled pass-through group composites isolated, so its effects
        // render (and its square still paints).
        let mut doc = Document::new(64, 64);
        let g = doc.add_group("g");
        let child_id = doc.alloc_id();
        let mut child = Layer::pixel(child_id, "sq");
        for y in 24..40 {
            for x in 24..40 {
                child
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::from_straight(0.9, 0.1, 0.1, 1.0));
            }
        }
        doc.layer_mut(g).unwrap().children_mut().unwrap().push(child);
        doc.layer_mut(g).unwrap().pass_through = true;
        doc.layer_mut(g).unwrap().effects = LayerEffects {
            drop_shadow: Some(ShadowFx {
                dx: 6.0,
                dy: 6.0,
                blur: 1.0,
                color: [0.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        assert!(out.get(44, 44).a > 0.5, "group shadow renders beside the square");
        let sq = straight(out.get(30, 30));
        assert!(sq[0] > 0.8, "group content still paints: {sq:?}");
    }

    #[test]
    fn bevel_lights_the_edge_facing_the_light() {
        use lumenply_doc::{BevelFx, LayerEffects};
        let mut doc = Document::new(80, 80);
        let id = doc.add_pixel_layer("box");
        for y in 20..60 {
            for x in 20..60 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.5, 0.5, 0.5, 1.0),
                );
            }
        }
        doc.layer_mut(id).unwrap().effects = LayerEffects {
            bevel: Some(BevelFx {
                size: 4.0,
                depth: 1.0,
                angle: 90.0, // light straight from above
                highlight: [1.0, 1.0, 1.0],
                shadow: [0.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        let top = straight(out.get(40, 22));
        let bottom = straight(out.get(40, 57));
        let centre = straight(out.get(40, 40));
        let left = straight(out.get(22, 40));
        assert!(top[0] > 0.75, "top edge lit: {top:?}");
        assert!(bottom[0] < 0.25, "bottom edge shaded: {bottom:?}");
        assert!(
            (centre[0] - 0.5).abs() < 0.03,
            "flat interior untouched: {centre:?}"
        );
        assert!(
            (left[0] - 0.5).abs() < 0.1,
            "side edges face across the light: {left:?}"
        );
        assert!(out.get(40, 15).a < 1e-4, "nothing paints outside");
    }

    #[test]
    fn overlays_paint_the_coverage_and_the_gradient_spans_it() {
        use lumenply_doc::{ColorOverlayFx, GradientOverlayFx, LayerEffects};
        let mut doc = Document::new(100, 60);
        let id = doc.add_pixel_layer("box");
        for y in 20..40 {
            for x in 10..90 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.2, 0.2, 1.0),
                );
            }
        }
        // Full-opacity colour overlay replaces the fill inside, leaves
        // the outside empty.
        doc.layer_mut(id).unwrap().effects = LayerEffects {
            color_overlay: Some(ColorOverlayFx {
                color: [0.8, 0.1, 0.3],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        let inside = straight(out.get(50, 30));
        assert!(
            close(inside[0], 0.8) && close(inside[2], 0.3),
            "overlay colour covers the fill: {inside:?}"
        );
        assert!(out.get(5, 30).a < 1e-4, "outside untouched");

        // Half opacity mixes with the fill: 0.5·0.8 + 0.5·0.2 = 0.5.
        doc.layer_mut(id).unwrap().effects.color_overlay = Some(ColorOverlayFx {
            color: [0.8, 0.1, 0.3],
            opacity: 0.5,
        });
        let out = composite_raster(&doc);
        let mixed = straight(out.get(50, 30));
        assert!(close(mixed[0], 0.5), "half-opacity mix: {mixed:?}");

        // A 0° gradient runs left → right across the content bounds:
        // start colour at the left edge, end colour at the right.
        doc.layer_mut(id).unwrap().effects = LayerEffects {
            gradient_overlay: Some(GradientOverlayFx {
                start: [1.0, 0.0, 0.0],
                end: [0.0, 0.0, 1.0],
                angle: 0.0,
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        let left = straight(out.get(11, 30));
        let right = straight(out.get(88, 30));
        let mid = straight(out.get(50, 30));
        assert!(
            left[0] > 0.95 && left[2] < 0.05,
            "left edge is the start: {left:?}"
        );
        assert!(
            right[2] > 0.95 && right[0] < 0.05,
            "right edge is the end: {right:?}"
        );
        assert!(
            (mid[0] - 0.5).abs() < 0.05 && (mid[2] - 0.5).abs() < 0.05,
            "middle mixes evenly: {mid:?}"
        );
        // 90° runs bottom → top: the top edge shows the end colour.
        doc.layer_mut(id).unwrap().effects.gradient_overlay = Some(GradientOverlayFx {
            start: [1.0, 0.0, 0.0],
            end: [0.0, 0.0, 1.0],
            angle: 90.0,
            opacity: 1.0,
        });
        let out = composite_raster(&doc);
        assert!(straight(out.get(50, 21))[2] > 0.9, "top is the end colour");
        assert!(straight(out.get(50, 38))[0] > 0.9, "bottom is the start colour");
    }

    #[test]
    fn inner_shadow_and_glow_stay_inside_the_coverage() {
        use lumenply_doc::{GlowFx, LayerEffects, ShadowFx};
        let mut doc = Document::new(96, 96);
        let id = doc.add_pixel_layer("square");
        for y in 30..66 {
            for x in 30..66 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.5, 0.5, 0.5, 1.0),
                );
            }
        }
        doc.layer_mut(id).unwrap().effects = LayerEffects {
            inner_glow: Some(GlowFx {
                blur: 4.0,
                color: [1.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        // Just inside the edge the glow tints strongly red; at the centre
        // the blurred inverse coverage has decayed to ~0.
        // At 1 px inside a step edge the blurred inverse coverage is ~0.36,
        // so red ≈ 0.36 × 1.0 + 0.64 × 0.5 = 0.68.
        let edge = straight(out.get(31, 48));
        let centre = straight(out.get(48, 48));
        assert!((edge[0] - 0.68).abs() < 0.06, "glow at the inside edge: {edge:?}");
        assert!(
            (centre[0] - 0.5).abs() < 0.02 && (centre[1] - 0.5).abs() < 0.02,
            "centre keeps the fill colour: {centre:?}"
        );
        // Nothing paints outside the coverage.
        assert!(out.get(28, 48).a < 1e-3, "outside stays empty");

        // Inner shadow with +dx/+dy darkens the top-left inside edge (the
        // outside creeps in from above-left), not the bottom-right one.
        doc.layer_mut(id).unwrap().effects = LayerEffects {
            inner_shadow: Some(ShadowFx {
                dx: 6.0,
                dy: 6.0,
                blur: 2.0,
                color: [0.0, 0.0, 0.0],
                opacity: 1.0,
            }),
            ..LayerEffects::default()
        };
        let out = composite_raster(&doc);
        let top = straight(out.get(48, 32));
        let bottom = straight(out.get(48, 63));
        assert!(top[0] < 0.1, "inner shadow under the top edge: {top:?}");
        assert!(
            (bottom[0] - 0.5).abs() < 0.05,
            "bottom edge keeps the fill: {bottom:?}"
        );
    }

    #[test]
    fn clip_chains_paint_only_inside_the_base() {
        let mut doc = Document::new(8, 8);
        let bg = doc.add_pixel_layer("bg");
        for y in 0..8 {
            for x in 0..8 {
                doc.layer_mut(bg).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.5, 0.5, 0.5, 1.0),
                );
            }
        }
        // Base: opaque on the left half, 50% at (4,4), empty on the right.
        let base = doc.add_pixel_layer("base");
        for y in 0..8 {
            for x in 0..4 {
                doc.layer_mut(base).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.6, 0.2, 1.0),
                );
            }
        }
        doc.layer_mut(base).unwrap().pixels_mut().unwrap().set_pixel(
            4,
            4,
            Rgba::from_straight(0.2, 0.6, 0.2, 0.5),
        );
        // Clipped: white everywhere, but shows only where the base is.
        let top = doc.add_pixel_layer("clipped");
        for y in 0..8 {
            for x in 0..8 {
                doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(1.0, 1.0, 1.0, 1.0),
                );
            }
        }
        doc.layer_mut(top).unwrap().clip = true;

        let out = composite_raster(&doc);
        let inside = straight(out.get(2, 2));
        assert!(
            close(inside[0], 1.0),
            "clipped paint shows on the base: {inside:?}"
        );
        let outside = straight(out.get(6, 2));
        assert!(
            close(outside[0], 0.5),
            "outside the base, the backdrop: {outside:?}"
        );
        // Coverage never grows: at the half-alpha base pixel the white
        // shows at 50% over the grey backdrop.
        let half = straight(out.get(4, 4));
        assert!(close(half[0], 0.75), "alpha forced to the base: {half:?}");

        // A clipped adjustment is gated the same way.
        doc.layer_mut(top).unwrap().visible = false;
        let adj = doc.add_adjustment(Adjustment::Invert);
        doc.layer_mut(adj).unwrap().clip = true;
        let out = composite_raster(&doc);
        let inv = lumenply_doc::adjust::srgb_decode(1.0 - lumenply_doc::adjust::srgb_encode(0.2));
        assert!(close(straight(out.get(2, 2))[0], inv), "inverted inside the base");
        assert!(close(straight(out.get(6, 2))[0], 0.5), "untouched outside");

        // Hiding the base hides its whole chain.
        doc.layer_mut(base).unwrap().visible = false;
        let out = composite_raster(&doc);
        assert!(
            close(straight(out.get(2, 2))[0], 0.5),
            "hidden base hides the chain"
        );

        // The base's opacity applies to the unit as a whole.
        doc.layer_mut(base).unwrap().visible = true;
        doc.layer_mut(adj).unwrap().visible = false;
        doc.layer_mut(top).unwrap().visible = true;
        doc.layer_mut(base).unwrap().opacity = 0.5;
        let out = composite_raster(&doc);
        assert!(close(straight(out.get(2, 2))[0], 0.75), "unit at half opacity");
    }

    #[test]
    fn pass_through_groups_reach_the_layers_below() {
        // A grey background with a group holding only an Invert adjustment.
        let mut doc = Document::new(8, 8);
        let bg = doc.add_pixel_layer("bg");
        for y in 0..8 {
            for x in 0..8 {
                doc.layer_mut(bg).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.2, 0.2, 1.0),
                );
            }
        }
        let g = doc.add_group("g");
        let id = doc.alloc_id();
        doc.layer_mut(g)
            .unwrap()
            .children_mut()
            .unwrap()
            .push(Layer::adjustment(id, Adjustment::Invert));

        // Invert runs in gamma space, so linear 0.2 maps to this:
        let inv = lumenply_doc::adjust::srgb_decode(1.0 - lumenply_doc::adjust::srgb_encode(0.2));

        // Isolated: the adjustment has nothing inside the group to act on.
        let p = straight(composite_raster(&doc).get(4, 4));
        assert!(close(p[0], 0.2), "isolated group leaves the backdrop: {p:?}");

        // Pass-through: it inverts the background.
        doc.layer_mut(g).unwrap().pass_through = true;
        let p = straight(composite_raster(&doc).get(4, 4));
        assert!(close(p[0], inv), "pass-through reaches below: {p:?}");

        // Group opacity scales the effect.
        doc.layer_mut(g).unwrap().opacity = 0.5;
        let p = straight(composite_raster(&doc).get(4, 4));
        assert!(
            close(p[0], (0.2 + inv) / 2.0),
            "half-strength pass-through: {p:?}"
        );

        // A mask gates where it applies.
        doc.layer_mut(g).unwrap().opacity = 1.0;
        let mut mask = lumenply_doc::Mask::hide_all();
        mask.fill_rect(Rect::new(0, 0, 4, 8), 1.0);
        doc.layer_mut(g).unwrap().mask = Some(mask);
        let out = composite_raster(&doc);
        assert!(close(straight(out.get(2, 4))[0], inv), "masked-in side inverted");
        assert!(
            close(straight(out.get(6, 4))[0], 0.2),
            "masked-out side untouched"
        );

        // A multiply child blends against the backdrop, not against an
        // isolated transparent buffer.
        let mut doc2 = Document::new(4, 4);
        let bg = doc2.add_pixel_layer("bg");
        doc2.layer_mut(bg).unwrap().pixels_mut().unwrap().set_pixel(
            1,
            1,
            Rgba::from_straight(0.5, 0.5, 0.5, 1.0),
        );
        let g = doc2.add_group("g");
        let cid = doc2.alloc_id();
        let mut child = Layer::pixel(cid, "mul");
        child
            .pixels_mut()
            .unwrap()
            .set_pixel(1, 1, Rgba::from_straight(0.5, 1.0, 0.0, 1.0));
        child.blend = BlendMode::Multiply;
        doc2.layer_mut(g).unwrap().children_mut().unwrap().push(child);
        doc2.layer_mut(g).unwrap().pass_through = true;
        let p = straight(composite_raster(&doc2).get(1, 1));
        assert!(
            close(p[0], 0.25) && close(p[1], 0.5),
            "multiply against backdrop: {p:?}"
        );
    }

    #[test]
    fn adjustment_layer_affects_only_what_is_below() {
        let mut doc = Document::new(2, 1);
        let bg = doc.add_pixel_layer("bg");
        let px = doc.layer_mut(bg).unwrap().pixels_mut().unwrap();
        px.set_pixel(0, 0, Rgba::from_straight(0.2, 0.2, 0.2, 1.0));
        px.set_pixel(1, 0, Rgba::from_straight(0.2, 0.2, 0.2, 0.5));
        let adj = doc.add_adjustment(Adjustment::Invert);
        let top = doc.add_pixel_layer("top");
        doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
            1,
            0,
            Rgba::from_straight(0.0, 0.0, 1.0, 1.0),
        );

        // Invert runs in gamma space, like Photoshop.
        let inv = lumenply_doc::adjust::srgb_decode(1.0 - lumenply_doc::adjust::srgb_encode(0.2));
        let out = composite_raster(&doc);
        let a = straight(out.get(0, 0));
        assert!(close(a[0], inv), "inverted below: {a:?}");
        let b = straight(out.get(1, 0));
        assert!(
            close(b[2], 1.0) && close(b[0], 0.0),
            "layer above untouched: {b:?}"
        );

        // Alpha survives inversion.
        doc.remove_layer(top);
        let out = composite_raster(&doc);
        let c = straight(out.get(1, 0));
        assert!(close(c[3], 0.5) && close(c[0], inv), "{c:?}");

        // Adjustment at half opacity blends halfway.
        doc.layer_mut(adj).unwrap().opacity = 0.5;
        let d = straight(composite_raster(&doc).get(0, 0));
        assert!(close(d[0], (0.2 + inv) / 2.0), "{d:?}");
    }

    #[test]
    fn masks_scale_coverage_and_default_outside_tiles() {
        let mut doc = Document::new(600, 4);
        let id = doc.add_pixel_layer("fill");
        let fill = Raster::filled(600, 4, Rgba::WHITE);
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);

        let mut mask = lumenply_doc::Mask::reveal_all();
        mask.set_value(10, 1, 0.25);
        doc.layer_mut(id).unwrap().mask = Some(mask);
        let out = composite_raster(&doc);
        assert!(close(out.get(10, 1).a, 0.25));
        assert!(close(out.get(11, 1).a, 1.0));
        assert!(close(out.get(500, 1).a, 1.0)); // tile 1 has no mask tile → default

        doc.layer_mut(id).unwrap().mask = Some(lumenply_doc::Mask::hide_all());
        assert_eq!(composite(&doc).len(), 0);
    }

    #[test]
    fn live_blur_layer_blurs_the_backdrop_across_tile_edges() {
        let mut doc = Document::new(600, 64);
        let id = doc.add_pixel_layer("edge");
        for x in 0..600 {
            let v = if x < 256 { 0.2 } else { 0.8 }; // hard edge exactly on a tile boundary
            for y in 0..64 {
                doc.layer_mut(id)
                    .unwrap()
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let before = composite_raster(&doc);
        assert!(close(before.get(255, 32).r, 0.2) && close(before.get(256, 32).r, 0.8));

        let f = doc.add_filter(lumenply_doc::Filter::GaussianBlur { radius: 6.0 });
        let after = composite_raster(&doc);
        let l = after.get(252, 32).r;
        let r = after.get(259, 32).r;
        assert!(
            l > 0.25 && l < 0.5 && r > 0.5 && r < 0.75,
            "blurred across the seam: {l} {r}"
        );
        assert!(close(after.get(20, 32).r, 0.2), "flat regions unchanged");
        assert!(close(after.get(20, 32).a, 1.0));
        assert!(
            close(after.get(0, 0).r, 0.2) && close(after.get(0, 0).a, 1.0),
            "canvas edge is clamped, not faded"
        );
        assert!(close(after.get(599, 63).r, 0.8) && close(after.get(599, 63).a, 1.0));

        doc.layer_mut(f).unwrap().opacity = 0.0;
        let off = composite_raster(&doc);
        assert!(
            close(off.get(252, 32).r, 0.2),
            "zero-opacity filter layer is a no-op"
        );

        // The layer's own pixels were never touched: the filter is live.
        assert!(close(
            doc.layer(id).unwrap().pixels().unwrap().get_pixel(255, 32).r,
            0.2
        ));
    }

    /// The tile-by-tile definition: what `render_tile` gives for each tile.
    fn per_tile(layers: &[Layer], rect: Rect, canvas: Rect) -> Vec<(TileCoord, Option<Vec<Rgba>>)> {
        rect.tiles()
            .into_iter()
            .map(|c| (c, render_tile(layers, c, canvas).map(|t| t.pixels().to_vec())))
            .collect()
    }

    fn staged(layers: &[Layer], rect: Rect, canvas: Rect) -> Vec<(TileCoord, Option<Vec<Rgba>>)> {
        let store = composite_layers(layers, rect, canvas);
        rect.tiles()
            .into_iter()
            .map(|c| (c, store.tile(c).map(|t| t.pixels().to_vec())))
            .collect()
    }

    /// A stack with every awkward neighbour a live filter can have: a base
    /// spilling past the canvas, filters of different reach, a masked and a
    /// half-opacity filter, a hidden one, a clip chain and an adjustment
    /// between filters, and a group holding its own filter.
    fn filter_stack() -> (Vec<Layer>, Rect) {
        use lumenply_doc::Filter as F;
        let canvas = Rect::new(0, 0, 700, 520);
        let mut base = Layer::pixel(1, "base");
        let px = base.pixels_mut().unwrap();
        for y in -20..540 {
            for x in -20..720 {
                let v = (((x * 7) ^ (y * 3)) & 255) as f32 / 255.0;
                px.set_pixel(
                    x,
                    y,
                    Rgba::new(
                        v,
                        v * 0.6,
                        1.0 - v,
                        if (x / 50 + y / 50) % 5 == 0 { 0.4 } else { 1.0 },
                    ),
                );
            }
        }
        let mut spot = Layer::pixel(2, "spot");
        for y in 200..330 {
            for x in 240..520 {
                spot.pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::new(0.9, 0.2, 0.1, 1.0));
            }
        }
        let mut clipped = Layer::pixel(3, "clipped");
        clipped.clip = true;
        for y in 0..520 {
            for x in (0..700).step_by(3) {
                clipped
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::new(0.1, 0.8, 0.3, 1.0));
            }
        }
        let mut masked = Layer::filter(5, F::BoxBlur { radius: 3.0 });
        let mut m = lumenply_doc::Mask::hide_all();
        for y in 100..400 {
            for x in 100..600 {
                m.set_value(x, y, ((x + y) % 7) as f32 / 6.0);
            }
        }
        masked.mask = Some(m);
        let mut half = Layer::filter(
            6,
            F::Sharpen {
                amount: 0.8,
                radius: 2.0,
            },
        );
        half.opacity = 0.5;
        let mut hidden = Layer::filter(7, F::Median { radius: 2.0 });
        hidden.visible = false;
        let mut inner_base = Layer::pixel(10, "inner");
        for y in 50..470 {
            for x in 60..640 {
                inner_base
                    .pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::new(0.3, 0.3, 0.9, 0.7));
            }
        }
        let group = Layer::with_content(
            11,
            "group",
            LayerContent::Group(vec![
                inner_base,
                Layer::filter(12, F::GaussianBlur { radius: 3.0 }),
            ]),
        );
        let layers = vec![
            base,
            Layer::filter(4, F::GaussianBlur { radius: 6.0 }),
            spot,
            clipped,
            masked,
            Layer::adjustment(8, lumenply_doc::Adjustment::Invert),
            half,
            hidden,
            group,
            Layer::filter(
                13,
                F::MotionBlur {
                    angle: 30.0,
                    distance: 9.0,
                },
            ),
        ];
        (layers, canvas)
    }

    #[test]
    fn staged_live_filters_match_the_tile_by_tile_definition_exactly() {
        let (layers, canvas) = filter_stack();
        let rects = [
            canvas,
            Rect::new(300, 100, 50, 300),
            Rect::new(600, 400, 300, 300),
        ];
        // Every prefix: each stage of the staged path is checked against the
        // per-tile path, whose own filters read stages already checked.
        for n in 1..=layers.len() {
            for rect in rects {
                let a = per_tile(&layers[..n], rect, canvas);
                let b = staged(&layers[..n], rect, canvas);
                assert_eq!(a.len(), b.len());
                for ((ca, ta), (cb, tb)) in a.iter().zip(&b) {
                    assert_eq!(ca, cb);
                    assert!(ta == tb, "{n} layers, {rect:?}, tile {ca:?} differs");
                }
            }
        }
        // And the stack really is filtered (not trivially empty).
        let full = staged(&layers, canvas, canvas);
        assert_eq!(
            full.iter().filter(|(_, t)| t.is_some()).count(),
            9,
            "3×3 tiles, all painted"
        );
    }

    #[test]
    #[ignore = "timing; cargo test --release -p lumenply-render stacked_live_filter_timing -- --ignored --nocapture"]
    fn stacked_live_filter_timing() {
        let mut doc = Document::new(1024, 768);
        let id = doc.add_pixel_layer("ramp");
        let px = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..768 {
            for x in 0..1024 {
                let v = ((x ^ y) & 255) as f32 / 255.0;
                px.set_pixel(x, y, Rgba::new(v, v * 0.5, 1.0 - v, 1.0));
            }
        }
        for n in 0..=3 {
            let t = std::time::Instant::now();
            let out = composite(&doc);
            println!("{n} stacked blur layers: {:?} ({} tiles)", t.elapsed(), out.len());
            doc.add_filter(lumenply_doc::Filter::GaussianBlur { radius: 4.0 });
        }
    }

    #[test]
    fn compact_storage_composites_identically_and_quickly() {
        // Build a document with every feature that reads tiles in the hot
        // path: a masked adjustment, a masked filter, a masked pixel layer.
        let mut doc = Document::new(512, 256);
        let bg = doc.add_pixel_layer("bg");
        let fill = Raster::filled(512, 256, Rgba::from_straight(0.3, 0.5, 0.7, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);
        let mut mask = lumenply_doc::Mask::reveal_all();
        gradient_mask(&mut mask, Rect::new(0, 0, 512, 256));
        doc.layer_mut(bg).unwrap().mask = Some(mask.clone());
        let adj = doc.add_adjustment(Adjustment::Invert);
        doc.layer_mut(adj).unwrap().mask = Some(mask.clone());
        let f = doc.add_filter(lumenply_doc::Filter::BoxBlur { radius: 3.0 });
        doc.layer_mut(f).unwrap().mask = Some(mask);
        let reference = composite_raster(&doc);

        let mut compact = doc.clone();
        compact.for_each_layer_mut(|l| {
            if let LayerContent::Pixel(s) = &mut l.content {
                s.compact();
            }
            if let Some(m) = l.mask.as_mut() {
                m.tiles.compact();
            }
        });
        let t = std::time::Instant::now();
        let out = composite_raster(&compact);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert!(ms < 5000.0, "compositing compact tiles took {ms} ms");
        let mut maxdiff = 0.0f32;
        for (a, b) in reference.pixels.iter().zip(out.pixels.iter()) {
            maxdiff = maxdiff.max((a.r - b.r).abs()).max((a.a - b.a).abs());
        }
        assert!(
            maxdiff < 2e-4,
            "compact storage changed the composite by {maxdiff}"
        );
    }

    #[test]
    fn hidden_layers_are_skipped_and_empty_canvas_is_sparse() {
        let mut doc = Document::new(600, 600);
        let id = doc.add_pixel_layer("x");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(10, 10, Rgba::WHITE);
        assert_eq!(composite(&doc).len(), 1);
        doc.layer_mut(id).unwrap().visible = false;
        assert_eq!(composite(&doc).len(), 0);
    }
}
