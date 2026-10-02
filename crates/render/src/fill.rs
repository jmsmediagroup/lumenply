//! Rendering fill layers into their pixel cache.
//!
//! A fill covers every tile that touches the canvas. A solid fill shares
//! one tile across the whole canvas (copy-on-write, so it costs one tile of
//! memory); a gradient renders each tile in parallel from a 1024-entry
//! colour table.

use std::sync::Arc;

use lumenply_doc::fill::FillSampler;
use lumenply_doc::{Document, Fill, FillLayer, Layer, LayerContent};
use lumenply_tiles::{Rect, Tile, TileStore, TILE_SIZE};
use rayon::prelude::*;

/// Render `fill` over exactly `canvas` (pixels outside it stay
/// transparent, so the layer's bounds are the canvas), compacted to 16-bit
/// unless `float` (the document's HDR mode) asks otherwise.
pub fn render_fill(fill: &Fill, canvas: Rect, float: bool) -> TileStore {
    let sampler = fill.sampler(canvas);
    let mut store = TileStore::new();
    let finish = |mut t: Tile| {
        if !float {
            t.compact();
        }
        Arc::new(t)
    };
    // Tiles wholly inside the canvas of a solid fill are all the same one.
    let shared = match &sampler {
        FillSampler::Solid(c) => Some(finish(Tile::filled(*c))),
        FillSampler::Gradient { .. } => None,
    };
    let tiles: Vec<_> = canvas
        .tiles()
        .into_par_iter()
        .map(|c| {
            let r = c.rect();
            let inside = r.intersect(&canvas) == r;
            if let (true, Some(s)) = (inside, &shared) {
                return (c, s.clone());
            }
            let (ox, oy) = c.origin();
            let mut t = Tile::new();
            for (i, p) in t.pixels_mut().iter_mut().enumerate() {
                let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
                if inside || canvas.contains(x, y) {
                    *p = sampler.sample(x, y);
                }
            }
            (c, finish(t))
        })
        .collect();
    for (c, t) in tiles {
        store.insert(c, t);
    }
    store
}

/// Re-render a fill layer's cache for a `width × height` canvas.
pub fn refresh_cache(f: &mut FillLayer, width: u32, height: u32, float: bool) {
    f.cache = Some(render_fill(&f.fill, Rect::new(0, 0, width, height), float));
    f.cache_canvas = (width, height);
}

/// Bring every fill layer's cache up to date with the document's canvas:
/// missing caches (a freshly loaded or built document) and caches rendered
/// for another canvas size are rebuilt; the rest are left alone. Shape
/// layers' caches are brought up to date too (see [`crate::shape`]).
pub fn refresh_stale(doc: &mut Document) {
    let (w, h, float) = (doc.width, doc.height, doc.float_mode);
    doc.for_each_layer_mut(|l: &mut Layer| {
        if let LayerContent::Fill(f) = &mut l.content {
            if f.is_stale(w, h) {
                refresh_cache(f, w, h, float);
            }
        }
    });
    crate::shape::refresh_stale(doc);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::adjust::srgb_encode;
    use lumenply_doc::{Gradient, GradientStyle};
    use lumenply_tiles::TileCoord;

    #[test]
    fn solid_fills_share_one_tile_over_the_canvas() {
        let s = render_fill(
            &Fill::Solid {
                color: [1.0, 0.5, 0.0],
            },
            Rect::new(0, 0, 600, 300),
            false,
        );
        assert_eq!(s.len(), 3 * 2, "every tile touching the canvas");
        assert_eq!(
            s.content_bounds(),
            Some(Rect::new(0, 0, 600, 300)),
            "exactly the canvas"
        );
        let a = s.tile_arc(TileCoord::new(0, 0)).unwrap();
        let b = s.tile_arc(TileCoord::new(1, 0)).unwrap();
        assert!(Arc::ptr_eq(a, b), "interior tiles share one tile");
        let p = s.get_pixel(599, 299);
        assert!((p.g - 0.5).abs() < 1e-4 && p.a == 1.0, "{p:?}");
        assert_eq!(s.get_pixel(600, 299).a, 0.0, "nothing past the edge");
    }

    #[test]
    fn gradient_fills_render_the_sampler_into_tiles() {
        let fill = Fill::Gradient {
            gradient: Gradient::default(),
            style: GradientStyle::Linear,
            angle: 0.0,
            scale: 1.0,
            reverse: false,
            offset: [0.0, 0.0],
        };
        let s = render_fill(&fill, Rect::new(0, 0, 512, 10), false);
        // Left edge near black, centre sRGB ~0.5, right edge near white.
        let enc = |x: i32| srgb_encode(s.get_pixel(x, 5).r);
        assert!(enc(0) < 0.01, "{}", enc(0));
        assert!((enc(255) - 0.499).abs() < 3e-3, "{}", enc(255));
        assert!(enc(511) > 0.99, "{}", enc(511));
        assert!(s.tile(TileCoord::new(0, 0)).unwrap().is_compact());
    }
}
