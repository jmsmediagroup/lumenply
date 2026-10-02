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

pub mod filters;
pub mod text;
pub mod transform;

pub use filters::{apply_filter, filter_raster, Filter};
pub use transform::{sample_bilinear, transform_mask, transform_store};

use nge_doc::{Adjustment, BlendMode, Document, Layer, LayerContent, Mask};
use nge_tiles::{Raster, Rect, Rgba, Tile, TileCoord, TileStore, TILE_PIXELS, TILE_SIZE};
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
    let tiles: Vec<(TileCoord, Option<Tile>)> = coords
        .into_par_iter()
        .map(|c| (c, render_tile(layers, c, canvas)))
        .collect();

    let mut out = TileStore::new();
    for (c, tile) in tiles {
        if let Some(t) = tile {
            out.insert(c, std::sync::Arc::new(t));
        }
    }
    out
}

/// Composite the whole canvas into a dense raster.
pub fn composite_raster(doc: &Document) -> Raster {
    composite(doc).to_raster(doc.canvas())
}

/// Render one tile of a layer stack. Returns `None` when nothing touches it.
pub fn render_tile(layers: &[Layer], coord: TileCoord, canvas: Rect) -> Option<Tile> {
    let mut dst: Option<Tile> = None;
    for (idx, layer) in layers.iter().enumerate() {
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
            LayerContent::Pixel(_) | LayerContent::Text(_) => {
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
            LayerContent::Group(children) => match render_tile(children, coord, canvas) {
                Some(t) => {
                    owned = t;
                    &owned
                }
                None => continue,
            },
            LayerContent::Adjustment(adj) => {
                // Adjustments modify the backdrop in place rather than
                // compositing over it, so alpha is never accumulated.
                if let Some(below) = dst.as_mut() {
                    adjust_in_place(below, adj, layer.blend, layer.opacity, mask, coord);
                }
                continue;
            }
            LayerContent::Filter(f) => {
                // A live filter needs the backdrop around this tile too, so
                // composite the layers below over the padded area, filter,
                // and keep the centre. Outside the canvas the edge pixel is
                // repeated, as Photoshop does, so borders don't fade.
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
                    continue;
                }
                let below = composite_layers(&layers[..idx], visible, canvas);
                if below.is_empty() && dst.is_none() {
                    continue;
                }
                let mut src = Raster::new(area.w, area.h);
                for y in 0..area.h as i32 {
                    let sy = (area.y + y).clamp(canvas.y, canvas.bottom() - 1);
                    for x in 0..area.w as i32 {
                        let sx = (area.x + x).clamp(canvas.x, canvas.right() - 1);
                        src.set(x as u32, y as u32, below.get_pixel(sx, sy));
                    }
                }
                let filtered = filter_raster(&src, f);
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
                continue;
            }
        };

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
    use nge_doc::Layer;

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

        let out = composite_raster(&doc);
        let a = straight(out.get(0, 0));
        assert!(close(a[0], 0.8), "inverted below: {a:?}");
        let b = straight(out.get(1, 0));
        assert!(
            close(b[2], 1.0) && close(b[0], 0.0),
            "layer above untouched: {b:?}"
        );

        // Alpha survives inversion.
        doc.remove_layer(top);
        let out = composite_raster(&doc);
        let c = straight(out.get(1, 0));
        assert!(close(c[3], 0.5) && close(c[0], 0.8), "{c:?}");

        // Adjustment at half opacity blends halfway.
        doc.layer_mut(adj).unwrap().opacity = 0.5;
        let d = straight(composite_raster(&doc).get(0, 0));
        assert!(close(d[0], 0.5), "{d:?}");
    }

    #[test]
    fn masks_scale_coverage_and_default_outside_tiles() {
        let mut doc = Document::new(600, 4);
        let id = doc.add_pixel_layer("fill");
        let fill = Raster::filled(600, 4, Rgba::WHITE);
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);

        let mut mask = nge_doc::Mask::reveal_all();
        mask.set_value(10, 1, 0.25);
        doc.layer_mut(id).unwrap().mask = Some(mask);
        let out = composite_raster(&doc);
        assert!(close(out.get(10, 1).a, 0.25));
        assert!(close(out.get(11, 1).a, 1.0));
        assert!(close(out.get(500, 1).a, 1.0)); // tile 1 has no mask tile → default

        doc.layer_mut(id).unwrap().mask = Some(nge_doc::Mask::hide_all());
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

        let f = doc.add_filter(nge_doc::Filter::GaussianBlur { radius: 6.0 });
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

    #[test]
    fn compact_storage_composites_identically_and_quickly() {
        // Build a document with every feature that reads tiles in the hot
        // path: a masked adjustment, a masked filter, a masked pixel layer.
        let mut doc = Document::new(512, 256);
        let bg = doc.add_pixel_layer("bg");
        let fill = Raster::filled(512, 256, Rgba::from_straight(0.3, 0.5, 0.7, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);
        let mut mask = nge_doc::Mask::reveal_all();
        gradient_mask(&mut mask, Rect::new(0, 0, 512, 256));
        doc.layer_mut(bg).unwrap().mask = Some(mask.clone());
        let adj = doc.add_adjustment(Adjustment::Invert);
        doc.layer_mut(adj).unwrap().mask = Some(mask.clone());
        let f = doc.add_filter(nge_doc::Filter::BoxBlur { radius: 3.0 });
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
