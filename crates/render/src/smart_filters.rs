//! Smart filters (ADR 0011): a layer's own filter stack, rendered into a
//! derived cache of filtered pixels that every compositor path reads through
//! [`Layer::raster_store`].
//!
//! The stack is computed in chunks of 4×4 tiles. A chunk reads the layer's
//! pixels over its area padded by the sum of the running filters' reaches
//! ([`SmartFilters::pad`]), runs every filter over that raster, and keeps
//! only its own tiles, which are exact: each filter reads at most its pad
//! around a pixel, so the error the raster's edge introduces shrinks inward
//! by one pad per filter and never reaches the chunk. Each filter treats
//! the canvas edge as destructive filters do (`apply_filter_in_canvas`):
//! off-canvas pixels the layer does not paint repeat the canvas edge and
//! stay empty afterwards. A smart filter therefore matches the same filters
//! applied destructively, one after another.
//!
//! After an edit only the source tiles that changed (compared by allocation
//! identity, see [`SmartFilterKey`]) are re-filtered, grown by the pad.

use std::collections::HashMap;
use std::sync::Arc;

use lumenply_doc::smart_filter::{SmartFilterCache, SmartFilterKey};
use lumenply_doc::{BlendMode, Document, Layer, Mask, SmartFilter, SmartFilters};
use lumenply_tiles::{Raster, Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;

use crate::filters::filter_raster;

/// Chunk side in tiles: big enough that the padding overhead stays small,
/// small enough to bound memory and spread over threads.
const CHUNK: i32 = 4;

fn identities(store: &TileStore) -> Vec<(TileCoord, usize)> {
    let mut v: Vec<(TileCoord, usize)> = store
        .coords()
        .filter_map(|c| store.tile(c).map(|t| (c, t as *const Tile as usize)))
        .collect();
    v.sort_unstable();
    v
}

/// What the cache of `sf` over `src` must have been rendered from to be
/// current.
pub fn cache_key(src: &TileStore, sf: &SmartFilters, canvas: Rect, float: bool) -> SmartFilterKey {
    SmartFilterKey {
        source: identities(src),
        filters: sf.active().cloned().collect(),
        mask: sf
            .mask
            .as_ref()
            .map(|m| (m.default, m.enabled, identities(&m.tiles))),
        canvas: (canvas.w, canvas.h),
        float,
    }
}

fn pad_rect(r: Rect, pad: i32) -> Rect {
    Rect::new(r.x - pad, r.y - pad, r.w + 2 * pad as u32, r.h + 2 * pad as u32)
}

/// `store` over `area` as a dense raster (transparent where it has no tile),
/// converting each tile once.
fn read_area(store: &TileStore, area: Rect) -> Raster {
    let mut out = Raster::new(area.w, area.h);
    let aw = area.w as usize;
    for c in area.tiles() {
        let Some(tile) = store.tile(c) else { continue };
        let px = tile.pixels();
        let (ox, oy) = c.origin();
        let sub = area.intersect(&c.rect());
        let len = sub.w as usize;
        for y in sub.y..sub.bottom() {
            let s = (y - oy) as usize * TILE_SIZE + (sub.x - ox) as usize;
            let d = (y - area.y) as usize * aw + (sub.x - area.x) as usize;
            out.pixels[d..d + len].copy_from_slice(&px[s..s + len]);
        }
    }
    out
}

/// The mask's coverage over `area`, row-major.
fn mask_area(m: &Mask, area: Rect) -> Vec<f32> {
    let aw = area.w as usize;
    let mut out = vec![m.default; aw * area.h as usize];
    for c in area.tiles() {
        let Some(tile) = m.tiles.tile(c) else { continue };
        let px = tile.pixels();
        let (ox, oy) = c.origin();
        let sub = area.intersect(&c.rect());
        for y in sub.y..sub.bottom() {
            for x in sub.x..sub.right() {
                out[(y - area.y) as usize * aw + (x - area.x) as usize] =
                    px[(y - oy) as usize * TILE_SIZE + (x - ox) as usize].a;
            }
        }
    }
    out
}

#[inline]
fn lerp(a: Rgba, b: Rgba, k: f32) -> Rgba {
    Rgba::new(
        a.r + (b.r - a.r) * k,
        a.g + (b.g - a.g) * k,
        a.b + (b.b - a.b) * k,
        a.a + (b.a - a.a) * k,
    )
}

/// One filter's result mixed onto its input by the filter's blending
/// options: Normal at 100 % is the filtered pixel itself; another mode
/// blends the filtered pixel onto the input first; opacity then mixes
/// between the input and that.
#[inline]
pub fn mix_filtered(input: Rgba, filtered: Rgba, mode: BlendMode, opacity: f32) -> Rgba {
    let target = if mode == BlendMode::Normal {
        filtered
    } else {
        crate::blend_pixel(input, filtered, mode, 1.0)
    };
    if opacity >= 1.0 {
        target
    } else {
        lerp(input, target, opacity.max(0.0))
    }
}

/// Run one smart filter over `input` (covering `area`), with the canvas
/// edge handled as `apply_filter_in_canvas` does.
fn run_one(input: &Raster, f: &SmartFilter, area: Rect, canvas: Rect) -> Raster {
    let aw = area.w as usize;
    let inside = canvas.intersect(&area) == area;
    let mut src = input.clone();
    if !inside && !canvas.is_empty() {
        for y in 0..area.h as i32 {
            for x in 0..aw as i32 {
                let (gx, gy) = (area.x + x, area.y + y);
                let i = y as usize * aw + x as usize;
                if canvas.contains(gx, gy) || input.pixels[i].a > 0.0 {
                    continue;
                }
                let (cx, cy) = (
                    gx.clamp(canvas.x, canvas.right() - 1),
                    gy.clamp(canvas.y, canvas.bottom() - 1),
                );
                if area.contains(cx, cy) {
                    src.pixels[i] = input.get((cx - area.x) as u32, (cy - area.y) as u32);
                }
            }
        }
    }
    let mut out = filter_raster(&src, &f.filter, (area.x, area.y));
    if !inside && !canvas.is_empty() {
        for (i, p) in out.pixels.iter_mut().enumerate() {
            let (gx, gy) = (area.x + (i % aw) as i32, area.y + (i / aw) as i32);
            if !canvas.contains(gx, gy) && input.pixels[i].a <= 0.0 {
                *p = Rgba::TRANSPARENT;
            }
        }
    }
    if f.opacity < 1.0 || f.blend != BlendMode::Normal {
        out.pixels
            .par_iter_mut()
            .zip(input.pixels.par_iter())
            .for_each(|(o, i)| *o = mix_filtered(*i, *o, f.blend, f.opacity));
    }
    out
}

/// The whole stack over `area`: exact within `area` shrunk by `sf.pad()`.
fn filter_area(src: &TileStore, sf: &SmartFilters, area: Rect, canvas: Rect) -> Raster {
    let orig = read_area(src, area);
    let mut cur: Option<Raster> = None;
    for f in sf.active() {
        cur = Some(run_one(cur.as_ref().unwrap_or(&orig), f, area, canvas));
    }
    let mut cur = cur.unwrap_or_else(|| orig.clone());
    if let Some(m) = sf.mask.as_ref().filter(|m| m.enabled) {
        let k = mask_area(m, area);
        cur.pixels
            .par_iter_mut()
            .zip(orig.pixels.par_iter().zip(k.par_iter()))
            .for_each(|(p, (o, k))| *p = lerp(*o, *p, *k));
    }
    cur
}

/// Render the tiles `wanted` of the filtered layer, `chunk` tiles a side per
/// pass. `None` marks a tile that came out blank.
fn render_tiles(
    src: &TileStore,
    sf: &SmartFilters,
    canvas: Rect,
    float: bool,
    wanted: &[TileCoord],
    chunk: i32,
) -> Vec<(TileCoord, Option<Arc<Tile>>)> {
    let pad = sf.pad();
    let mut chunks: HashMap<(i32, i32), Vec<TileCoord>> = HashMap::new();
    for &c in wanted {
        chunks
            .entry((c.x.div_euclid(chunk), c.y.div_euclid(chunk)))
            .or_default()
            .push(c);
    }
    let chunks: Vec<Vec<TileCoord>> = chunks.into_values().collect();
    chunks
        .into_par_iter()
        .flat_map_iter(|tiles| {
            let bounds = tiles
                .iter()
                .map(|c| c.rect())
                .reduce(|a, b| a.union(&b))
                .expect("chunks are never empty");
            let area = pad_rect(bounds, pad);
            // Nothing to read: every tile stays blank.
            let empty = area.tiles().iter().all(|c| src.tile(*c).is_none());
            let raster = (!empty).then(|| filter_area(src, sf, area, canvas));
            tiles.into_iter().map(move |c| {
                let Some(r) = raster.as_ref() else {
                    return (c, None);
                };
                let (ox, oy) = c.origin();
                let mut t = Tile::new();
                let aw = area.w as usize;
                {
                    let px = t.pixels_mut();
                    for y in 0..TILE_SIZE {
                        let s = (oy - area.y) as usize * aw + y * aw + (ox - area.x) as usize;
                        px[y * TILE_SIZE..(y + 1) * TILE_SIZE].copy_from_slice(&r.pixels[s..s + TILE_SIZE]);
                    }
                }
                if t.is_blank() {
                    return (c, None);
                }
                if !float {
                    t.compact();
                }
                (c, Some(Arc::new(t)))
            })
        })
        .collect()
}

/// The tiles a full render of `src` can paint: its tiles grown by the pad.
fn full_extent(src: &TileStore, pad: i32) -> Vec<TileCoord> {
    match src.bounds() {
        Some(b) => pad_rect(b, pad).tiles(),
        None => Vec::new(),
    }
}

fn build(tiles: Vec<(TileCoord, Option<Arc<Tile>>)>, mut into: TileStore) -> TileStore {
    for (c, t) in tiles {
        match t {
            Some(t) => into.insert(c, t),
            None => {
                into.remove(c);
            }
        }
    }
    into
}

/// The filtered pixels of `src` under `sf`, rendered from scratch.
pub fn render_smart_filters(src: &TileStore, sf: &SmartFilters, canvas: Rect, float: bool) -> TileStore {
    render_chunked(src, sf, canvas, float, CHUNK)
}

fn render_chunked(src: &TileStore, sf: &SmartFilters, canvas: Rect, float: bool, chunk: i32) -> TileStore {
    if !sf.is_active() {
        return src.clone();
    }
    let wanted = full_extent(src, sf.pad());
    build(
        render_tiles(src, sf, canvas, float, &wanted, chunk),
        TileStore::new(),
    )
}

/// Does only the source differ between the two keys?
fn same_settings(a: &SmartFilterKey, b: &SmartFilterKey) -> bool {
    a.filters == b.filters && a.mask == b.mask && a.canvas == b.canvas && a.float == b.float
}

/// The canvas area whose source tiles differ between the keys.
fn changed_area(old: &SmartFilterKey, new: &SmartFilterKey) -> Option<Rect> {
    let before: HashMap<TileCoord, usize> = old.source.iter().copied().collect();
    let after: HashMap<TileCoord, usize> = new.source.iter().copied().collect();
    let mut acc: Option<Rect> = None;
    let mut add = |c: TileCoord| acc = Some(acc.map_or(c.rect(), |a| a.union(&c.rect())));
    for (c, a) in &after {
        if before.get(c) != Some(a) {
            add(*c);
        }
    }
    for c in before.keys() {
        if !after.contains_key(c) {
            add(*c);
        }
    }
    acc
}

/// Bring one layer's filtered cache up to date: nothing when it is
/// current, only the neighbourhood of changed tiles when just the pixels
/// changed, everything otherwise. Inactive stacks drop their cache.
pub fn refresh_layer(layer: &mut Layer, canvas: Rect, float: bool) {
    let next = {
        let sf = &layer.smart_filters;
        match layer.content_store() {
            Some(src) if sf.is_active() => {
                let key = cache_key(src, sf, canvas, float);
                match &sf.cache {
                    Some(c) if c.key == key => return,
                    Some(old) if same_settings(&old.key, &key) => {
                        let store = match changed_area(&old.key, &key) {
                            None => old.store.clone(),
                            Some(dirty) => {
                                let wanted = pad_rect(dirty, sf.pad()).tiles();
                                let tiles = render_tiles(src, sf, canvas, float, &wanted, CHUNK);
                                build(tiles, old.store.clone())
                            }
                        };
                        Some(SmartFilterCache { store, key })
                    }
                    _ => Some(SmartFilterCache {
                        store: render_smart_filters(src, sf, canvas, float),
                        key,
                    }),
                }
            }
            _ => None,
        }
    };
    layer.smart_filters.cache = next;
}

/// Refresh every layer's smart-filter cache (see [`refresh_layer`]). Run by
/// the editor after every command and by loaders, after the text, fill,
/// shape and smart-object caches it reads.
pub fn refresh_stale(doc: &mut Document) {
    let (canvas, float) = (doc.canvas(), doc.float_mode);
    doc.for_each_layer_mut(|l| {
        if !l.smart_filters.is_empty() || l.smart_filters.cache.is_some() {
            refresh_layer(l, canvas, float);
        }
    });
}

/// The largest smart-filter reach in the document: an edit to a layer's
/// pixels can change its filtered look this far around the edit.
pub fn max_pad(doc: &Document) -> i32 {
    let mut p = 0;
    doc.for_each_layer(|l| p = p.max(l.smart_filters.pad()));
    p
}

/// Bake a layer's smart filters into `store` (its own pixels), for
/// rasterizing and for exporters that cannot keep them live.
pub fn bake(store: &TileStore, sf: &SmartFilters, canvas: Rect, float: bool) -> TileStore {
    render_smart_filters(store, sf, canvas, float)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_filter_in_canvas;
    use lumenply_doc::Filter;

    /// A 128×64 canvas: white left of x = 64, black from there on.
    fn step_store(w: i32, h: i32, edge: i32) -> TileStore {
        let mut r = Raster::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                let v = if x < edge { 1.0 } else { 0.0 };
                r.set(x as u32, y as u32, Rgba::new(v, v, v, 1.0));
            }
        }
        TileStore::from_raster(&r, 0, 0)
    }

    fn stack(filters: &[Filter]) -> SmartFilters {
        SmartFilters {
            filters: filters.iter().cloned().map(SmartFilter::new).collect(),
            ..SmartFilters::default()
        }
    }

    fn max_diff(a: &TileStore, b: &TileStore, area: Rect) -> f32 {
        let mut d = 0f32;
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
                d = d
                    .max((p.r - q.r).abs())
                    .max((p.g - q.g).abs())
                    .max((p.b - q.b).abs())
                    .max((p.a - q.a).abs());
            }
        }
        d
    }

    #[test]
    fn box_blur_smart_filter_has_the_expected_ramp() {
        let canvas = Rect::new(0, 0, 128, 64);
        let src = step_store(128, 64, 64);
        let out = render_smart_filters(&src, &stack(&[Filter::BoxBlur { radius: 2.0 }]), canvas, true);
        // A 5-px window: x = 64 sees two white pixels (62, 63) of five.
        let at = |x| out.get_pixel(x, 32).r;
        assert!((at(64) - 0.4).abs() < 1e-6, "{}", at(64));
        assert!((at(63) - 0.6).abs() < 1e-6);
        assert!((at(65) - 0.2).abs() < 1e-6);
        assert_eq!(at(10), 1.0);
        assert_eq!(at(100), 0.0);
        // The canvas edge repeats, so the corner stays opaque white.
        assert_eq!(out.get_pixel(0, 0), Rgba::new(1.0, 1.0, 1.0, 1.0));
        // Nothing leaks off the canvas.
        assert_eq!(out.get_pixel(-1, 10), Rgba::TRANSPARENT);
    }

    #[test]
    fn order_matters_and_each_order_equals_destructive_filters() {
        let canvas = Rect::new(0, 0, 128, 64);
        let src = step_store(128, 64, 64);
        let mosaic = Filter::Mosaic { size: 4.0 };
        let blur = Filter::BoxBlur { radius: 2.0 };
        let mb = render_smart_filters(&src, &stack(&[mosaic.clone(), blur.clone()]), canvas, true);
        let bm = render_smart_filters(&src, &stack(&[blur.clone(), mosaic.clone()]), canvas, true);
        // Mosaic first: cells align with the edge, so the blur ramp stays.
        assert!((mb.get_pixel(64, 32).r - 0.4).abs() < 1e-6);
        // Blur first: the cell 64..68 averages 0.4, 0.2, 0, 0.
        assert!((bm.get_pixel(64, 32).r - 0.15).abs() < 1e-6);
        // The cell 60..64 averages 1, 1, 0.8, 0.6.
        assert!((bm.get_pixel(61, 32).r - 0.85).abs() < 1e-6);

        let reference = |a: &Filter, b: &Filter| {
            let once = apply_filter_in_canvas(&src, a, canvas);
            apply_filter_in_canvas(&once, b, canvas)
        };
        assert!(max_diff(&mb, &reference(&mosaic, &blur), canvas) < 1e-4);
        assert!(max_diff(&bm, &reference(&blur, &mosaic), canvas) < 1e-4);
    }

    #[test]
    fn chunks_have_no_seams() {
        // Edges across tile boundaries and close to the canvas border; every
        // tile computed as its own chunk must match one pass over the layer.
        let canvas = Rect::new(0, 0, 600, 530);
        let mut r = Raster::new(600, 530);
        for y in 0..530 {
            for x in 0..600 {
                let on = (x / 37 + y / 23) % 2 == 0 || (250..262).contains(&x);
                let v = if on { 0.9 } else { 0.1 };
                r.set(x as u32, y as u32, Rgba::new(v, v * 0.5, 1.0 - v, 1.0));
            }
        }
        let src = TileStore::from_raster(&r, 0, 0);
        let filters = [
            Filter::GaussianBlur { radius: 8.0 },
            Filter::Emboss {
                angle: 30.0,
                height: 3.0,
                amount: 1.0,
            },
        ];
        let sf = stack(&filters);
        let reference = apply_filter_in_canvas(
            &apply_filter_in_canvas(&src, &filters[0], canvas),
            &filters[1],
            canvas,
        );
        for chunk in [1, 2, CHUNK] {
            let out = render_chunked(&src, &sf, canvas, true, chunk);
            let d = max_diff(&out, &reference, Rect::new(-8, -8, 616, 546));
            assert!(d < 1e-4, "chunk {chunk}: {d}");
        }
    }

    #[test]
    fn opacity_blend_and_mask_mix_with_the_unfiltered_pixels() {
        let canvas = Rect::new(0, 0, 128, 64);
        let src = step_store(128, 64, 64);
        let mut sf = stack(&[Filter::BoxBlur { radius: 2.0 }]);
        sf.filters[0].opacity = 0.5;
        let half = render_smart_filters(&src, &sf, canvas, true);
        // Black input 0, filtered 0.4: halfway is 0.2.
        assert!((half.get_pixel(64, 32).r - 0.2).abs() < 1e-6);
        sf.filters[0].opacity = 1.0;
        sf.filters[0].blend = BlendMode::Darken;
        let dark = render_smart_filters(&src, &sf, canvas, true);
        // Darken keeps the darker of input and filtered.
        assert!(dark.get_pixel(64, 32).r.abs() < 1e-6);
        assert!((dark.get_pixel(63, 32).r - 0.6).abs() < 1e-6);
        sf.filters[0].blend = BlendMode::Normal;
        // A filter mask hiding the top half shows the unfiltered step there.
        let mut m = Mask::reveal_all();
        for y in 0..32 {
            for x in 56..72 {
                m.set_value(x, y, 0.0);
            }
        }
        m.set_value(64, 40, 0.25);
        sf.mask = Some(m);
        let masked = render_smart_filters(&src, &sf, canvas, true);
        assert_eq!(masked.get_pixel(64, 10).r, 0.0);
        assert_eq!(masked.get_pixel(63, 10).r, 1.0);
        assert!((masked.get_pixel(64, 50).r - 0.4).abs() < 1e-6);
        assert!((masked.get_pixel(64, 40).r - 0.1).abs() < 1e-6);
    }

    #[test]
    fn layer_cache_follows_switches_and_edits_incrementally() {
        let canvas = Rect::new(0, 0, 1400, 300);
        let mut layer = Layer::pixel(1, "L");
        *layer.pixels_mut().unwrap() = step_store(1400, 300, 64);
        layer
            .smart_filters
            .filters
            .push(SmartFilter::new(Filter::GaussianBlur { radius: 4.0 }));
        refresh_layer(&mut layer, canvas, false);
        assert!(layer.smart_filters.cache.is_some());
        let filtered = layer.raster_store().unwrap().get_pixel(64, 100).r;
        assert!(filtered > 0.2 && filtered < 0.6, "{filtered}");
        assert_eq!(layer.content_store().unwrap().get_pixel(64, 100).r, 0.0);

        // Disabling restores the plain pixels and drops the cache.
        layer.smart_filters.filters[0].enabled = false;
        refresh_layer(&mut layer, canvas, false);
        assert!(layer.smart_filters.cache.is_none());
        assert_eq!(layer.raster_store().unwrap().get_pixel(64, 100).r, 0.0);
        layer.smart_filters.filters[0].enabled = true;
        refresh_layer(&mut layer, canvas, false);

        // Paint copy-on-write into the tile at x 1024..1280 (as the editor
        // does: the old store stays alive meanwhile).
        let before = layer.smart_filters.cache.clone().unwrap();
        let old_pixels = layer.pixels().unwrap().clone();
        {
            let px = layer.pixels_mut().unwrap();
            for y in 110..150 {
                for x in 1010..1040 {
                    px.set_pixel(x, y, Rgba::new(1.0, 0.0, 0.0, 1.0));
                }
            }
        }
        refresh_layer(&mut layer, canvas, false);
        let after = &layer.smart_filters.cache.as_ref().unwrap().store;
        // Far tiles are reused as they were; the painted area is filtered.
        let far = TileCoord::new(0, 0);
        assert!(std::ptr::eq(
            before.store.tile(far).unwrap(),
            after.tile(far).unwrap()
        ));
        assert!(after.get_pixel(1025, 130).r > 0.9);
        let full = render_smart_filters(layer.pixels().unwrap(), &layer.smart_filters, canvas, false);
        assert!(max_diff(after, &full, canvas) < 1e-4);
        drop(old_pixels);
    }
    #[test]
    fn composite_matches_destructive_filtering_and_spares_layers_below() {
        // Bottom: a horizontal ramp. Top: an orange square, blurred by a
        // smart filter, once as a pixel layer and once as a smart object.
        let canvas = Rect::new(0, 0, 300, 280);
        let mut ramp = Raster::new(300, 280);
        for y in 0..280 {
            for x in 0..300 {
                let v = x as f32 / 299.0;
                ramp.set(x as u32, y as u32, Rgba::new(v, 0.2, 1.0 - v, 1.0));
            }
        }
        let mut square = Raster::new(300, 280);
        for y in 200..270 {
            for x in 230..290 {
                square.set(x as u32, y as u32, Rgba::new(1.0, 0.5, 0.0, 1.0));
            }
        }
        let blur = Filter::GaussianBlur { radius: 6.0 };
        let top_px = TileStore::from_raster(&square, 0, 0);
        for smart in [false, true] {
            let mut doc = Document::new(300, 280);
            let bottom = doc.add_pixel_layer("ramp");
            *doc.layer_mut(bottom).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&ramp, 0, 0);
            let top = doc.add_pixel_layer("square");
            {
                let l = doc.layer_mut(top).unwrap();
                if smart {
                    l.content = lumenply_doc::LayerContent::Smart(lumenply_doc::SmartLayer {
                        source: top_px.clone(),
                        transform: lumenply_tiles::Affine::IDENTITY,
                        cache: Some(top_px.clone()),
                    });
                } else {
                    *l.pixels_mut().unwrap() = top_px.clone();
                }
                l.smart_filters.filters.push(SmartFilter::new(blur.clone()));
            }
            let plain = crate::composite(&doc);
            crate::fill::refresh_stale(&mut doc);
            let live = crate::composite(&doc);

            let mut baked = Document::new(300, 280);
            let b = baked.add_pixel_layer("ramp");
            *baked.layer_mut(b).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&ramp, 0, 0);
            let t = baked.add_pixel_layer("square");
            *baked.layer_mut(t).unwrap().pixels_mut().unwrap() =
                apply_filter_in_canvas(&top_px, &blur, canvas);
            let reference = crate::composite(&baked);
            assert!(max_diff(&live, &reference, canvas) < 1e-4, "smart {smart}");
            // The blur spreads past the square (pad 9 px) ...
            assert!(live.get_pixel(225, 235) != plain.get_pixel(225, 235));
            // ... but the ramp elsewhere is untouched, and so is its layer.
            assert_eq!(live.get_pixel(100, 100), plain.get_pixel(100, 100));
            let r = live.get_pixel(100, 100);
            assert!((r.r - 100.0 / 299.0).abs() < 1e-4 && (r.g - 0.2).abs() < 1e-4);
            assert!(doc.layer(bottom).unwrap().smart_filters.cache.is_none());
        }
    }
}
