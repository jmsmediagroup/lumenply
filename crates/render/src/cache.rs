//! Per-layer composite caching.
//!
//! While the user edits one layer (a stroke, an opacity drag, adjustment
//! sliders), everything *below* that layer's top-level ancestor is static.
//! [`BelowCache`] keeps that backdrop composited per tile, so recompositing
//! costs only the layers from the edited one upwards. The cache is driven
//! by [`BelowCache::note_change`]; a change it cannot attribute to the same
//! top-level layer drops the cache, so the output always equals the
//! reference compositor (tested below).

use std::collections::HashMap;
use std::sync::Arc;

use nge_doc::{Document, Layer, LayerContent, LayerId};
use nge_tiles::{Rect, Tile, TileCoord, TileStore};
use rayon::prelude::*;

use crate::{composite_layers, render_tile, render_tile_over};

/// Cached composite of the top-level layers below the one being edited.
/// Holds at most one backdrop tile per canvas tile (f32, so roughly
/// 1 MB per 256×256 tile); cleared whenever the edit target or the canvas
/// changes.
#[derive(Default)]
pub struct BelowCache {
    /// Top-level layer whose backdrop is cached.
    key: Option<LayerId>,
    canvas: Rect,
    /// `None` entries mean "nothing below touches this tile".
    tiles: HashMap<TileCoord, Option<Arc<Tile>>>,
}

impl BelowCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of backdrop tiles currently cached (for tests and stats).
    pub fn cached_tiles(&self) -> usize {
        self.tiles.len()
    }

    /// Tell the cache which layer an edit touched (`None` = anything).
    /// Consecutive edits inside the same top-level layer keep the backdrop.
    pub fn note_change(&mut self, doc: &Document, changed: Option<LayerId>) {
        let key = changed.and_then(|id| top_ancestor(doc, id));
        if key != self.key || key.is_none() {
            self.key = key;
            self.tiles.clear();
        }
    }

    /// Composite `rect` of the document, reusing the cached backdrop when
    /// possible. Always equal to [`crate::composite_rect`].
    pub fn composite_rect(&mut self, doc: &Document, rect: Rect) -> TileStore {
        let canvas = doc.canvas();
        if self.canvas != canvas {
            self.canvas = canvas;
            self.tiles.clear();
        }
        let layers = doc.layers();
        let split = self.key.and_then(|k| layers.iter().position(|l| l.id == k));
        // A clip chain composites as one unit with its base: splitting in
        // the middle would bake the base into the backdrop without its
        // clipped companions. Walk down to the chain's base.
        let split = split.map(|mut s| {
            while s > 0 && layers[s].clip {
                s -= 1;
            }
            s
        });
        let Some(split) = split.filter(|s| *s > 0) else {
            return composite_layers(layers, rect, canvas);
        };
        // A live filter at or above the split reads its backdrop from the
        // layer slice, which the cached backdrop would bypass: take the
        // reference path for correctness.
        let filter_above = layers[split..]
            .iter()
            .any(|l| l.visible && l.opacity > 0.0 && matches!(l.content, LayerContent::Filter(_)));
        if filter_above {
            return composite_layers(layers, rect, canvas);
        }

        let coords = rect.tiles();
        let missing: Vec<TileCoord> = coords
            .iter()
            .filter(|c| !self.tiles.contains_key(c))
            .copied()
            .collect();
        let computed: Vec<(TileCoord, Option<Tile>)> = missing
            .into_par_iter()
            .map(|c| (c, render_tile(&layers[..split], c, canvas)))
            .collect();
        for (c, t) in computed {
            self.tiles.insert(c, t.map(Arc::new));
        }

        let tiles: Vec<(TileCoord, Option<Tile>)> = coords
            .into_par_iter()
            .map(|c| {
                let backdrop = self.tiles.get(&c).and_then(|o| o.as_ref()).map(|t| (**t).clone());
                (c, render_tile_over(backdrop, &layers[split..], c, canvas))
            })
            .collect();
        let mut out = TileStore::new();
        for (c, tile) in tiles {
            if let Some(t) = tile {
                out.insert(c, Arc::new(t));
            }
        }
        out
    }
}

/// The top-level layer containing `id` (possibly `id` itself).
fn top_ancestor(doc: &Document, id: LayerId) -> Option<LayerId> {
    fn contains(l: &Layer, id: LayerId) -> bool {
        l.id == id || l.children().is_some_and(|c| c.iter().any(|ch| contains(ch, id)))
    }
    doc.layers().iter().find(|l| contains(l, id)).map(|l| l.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{composite_rect, gradient_mask};
    use nge_doc::{Adjustment, BlendMode, Mask};
    use nge_tiles::{Raster, Rgba, TileStore as Store};

    /// A document exercising every compositor feature below the top layer:
    /// a filled background, a masked multiply layer, an adjustment, a live
    /// blur, a group — and a plain top layer to edit.
    fn busy_doc() -> (Document, LayerId) {
        let mut doc = Document::new(700, 400);
        let bg = doc.add_pixel_layer("bg");
        let fill = Raster::filled(700, 400, Rgba::from_straight(0.6, 0.5, 0.4, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = Store::from_raster(&fill, 0, 0);

        let mul = doc.add_pixel_layer("mul");
        let fill = Raster::filled(700, 400, Rgba::from_straight(0.8, 0.8, 0.9, 0.7));
        *doc.layer_mut(mul).unwrap().pixels_mut().unwrap() = Store::from_raster(&fill, 0, 0);
        let l = doc.layer_mut(mul).unwrap();
        l.blend = BlendMode::Multiply;
        l.opacity = 0.8;
        let mut mask = Mask::reveal_all();
        gradient_mask(&mut mask, Rect::new(0, 0, 700, 400));
        l.mask = Some(mask);

        doc.add_adjustment(Adjustment::BrightnessContrast {
            brightness: 0.1,
            contrast: 0.2,
        });
        doc.add_filter(nge_doc::Filter::BoxBlur { radius: 2.0 });

        let g = doc.add_group("g");
        let inner_id = doc.alloc_id();
        let mut inner = Layer::pixel(inner_id, "inner");
        inner
            .pixels_mut()
            .unwrap()
            .set_pixel(300, 200, Rgba::from_straight(0.1, 0.9, 0.1, 1.0));
        doc.layer_mut(g).unwrap().children_mut().unwrap().push(inner);

        let top = doc.add_pixel_layer("top");
        doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
            100,
            100,
            Rgba::from_straight(1.0, 0.0, 0.0, 0.9),
        );
        (doc, top)
    }

    fn assert_equal(a: &TileStore, b: &TileStore, canvas: Rect, what: &str) {
        let (ra, rb) = (a.to_raster(canvas), b.to_raster(canvas));
        let mut maxdiff = 0.0f32;
        for (p, q) in ra.pixels.iter().zip(rb.pixels.iter()) {
            for (x, y) in [(p.r, q.r), (p.g, q.g), (p.b, q.b), (p.a, q.a)] {
                maxdiff = maxdiff.max((x - y).abs());
            }
        }
        assert!(maxdiff < 1e-6, "{what}: differs from reference by {maxdiff}");
    }

    #[test]
    fn cached_composite_matches_the_reference() {
        let (mut doc, top) = busy_doc();
        let canvas = doc.canvas();
        let mut cache = BelowCache::new();

        // Cold, keyed to the top layer.
        cache.note_change(&doc, Some(top));
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "cold");
        let warmed = cache.cached_tiles();
        assert!(warmed > 0, "backdrop was cached");

        // Edit the top layer: the backdrop is reused, output stays exact.
        doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
            500,
            300,
            Rgba::from_straight(0.0, 0.0, 1.0, 1.0),
        );
        doc.layer_mut(top).unwrap().opacity = 0.6;
        cache.note_change(&doc, Some(top));
        assert_eq!(cache.cached_tiles(), warmed, "cache survives same-layer edits");
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "warm");

        // Edit a layer below: the cache re-keys and stays exact.
        let bg = doc.layers()[0].id;
        doc.layer_mut(bg)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(10, 10, Rgba::WHITE);
        cache.note_change(&doc, Some(bg));
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "re-keyed");

        // A structural change (None) clears the cache and stays exact.
        cache.note_change(&doc, None);
        assert_eq!(cache.cached_tiles(), 0);
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "cleared");
    }

    #[test]
    fn filter_layer_above_the_split_falls_back_to_the_reference_path() {
        let (mut doc, _top) = busy_doc();
        // Move the edit target below the live filter: key to the masked
        // multiply layer, so the blur sits above the split.
        let mul = doc.layers()[1].id;
        let canvas = doc.canvas();
        let mut cache = BelowCache::new();
        cache.note_change(&doc, Some(mul));
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "filter above");
        assert_eq!(cache.cached_tiles(), 0, "fallback path caches nothing");

        // Editing inside a group keys to the group itself.
        let g = doc.layers()[4].id;
        let inner = doc.layer(g).unwrap().children().unwrap()[0].id;
        cache.note_change(&doc, Some(inner));
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "group member");
        assert!(cache.cached_tiles() > 0, "group backdrop cached");
        doc.layer_mut(g).unwrap().opacity = 0.5;
        cache.note_change(&doc, Some(g));
        let out = cache.composite_rect(&doc, canvas);
        assert_equal(&out, &composite_rect(&doc, canvas), canvas, "group props");
    }

    #[test]
    fn canvas_resize_drops_the_cache() {
        let (doc, top) = busy_doc();
        let mut cache = BelowCache::new();
        cache.note_change(&doc, Some(top));
        cache.composite_rect(&doc, doc.canvas());
        assert!(cache.cached_tiles() > 0);

        let mut small = doc.clone();
        small.width = 300;
        small.height = 200;
        cache.note_change(&small, Some(top));
        let out = cache.composite_rect(&small, small.canvas());
        assert_equal(
            &out,
            &composite_rect(&small, small.canvas()),
            small.canvas(),
            "resized",
        );
    }
}
