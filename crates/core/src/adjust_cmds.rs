//! Image ▸ Adjustments that bake into a pixel layer, as Photoshop's do:
//! Shadows/Highlights, Equalize, Desaturate, Replace Color and Match
//! Color. Each is one undo step, acts inside the selection (blended by its
//! coverage) and leaves layer locks to the editor. The pixel maths lives
//! in [`lumenply_render::adjust_more`].

use lumenply_doc::{Document, LayerId, Selection};
use lumenply_render::adjust_more::{
    self, histogram_add, map_store_pixels, LabStats, MatchColor, ReplaceColor, ShadowsHighlights, EQ_LEVELS,
};
use lumenply_tiles::{Rect, Rgba, TileStore};

use crate::{Command, EditError, EditResult};

pub use lumenply_render::adjust_more::{LabStats as MatchStats, ToneZone};

#[inline]
fn lerp(o: Rgba, f: Rgba, k: f32) -> Rgba {
    Rgba::new(
        o.r + (f.r - o.r) * k,
        o.g + (f.g - o.g) * k,
        o.b + (f.b - o.b) * k,
        o.a + (f.a - o.a) * k,
    )
}

/// The canvas area a selection can touch (everything without one).
fn selection_limit(sel: Option<&Selection>, canvas: Rect) -> Option<Rect> {
    sel.map(|s| s.tight_bounds(canvas))
}

/// Replace a pixel layer's pixels by `f(x, y, pixel)`, blended over the
/// original by the selection's coverage when there is a selection.
fn edit_pixels(doc: &mut Document, layer: LayerId, f: impl Fn(i32, i32, Rgba) -> Rgba + Sync) -> EditResult {
    let sel = doc.selection.clone();
    let canvas = doc.canvas();
    let limit = selection_limit(sel.as_ref(), canvas);
    let l = doc.layer_mut(layer).ok_or(EditError::NoLayer(layer))?;
    let store = l.pixels_mut().ok_or(EditError::NotPixel(layer))?;
    let out = map_store_pixels(store, limit, |x, y, p| {
        let k = sel.as_ref().map_or(1.0, |s| s.value(x, y));
        if k <= 0.0 {
            return p;
        }
        let q = f(x, y, p);
        if k >= 1.0 {
            q
        } else {
            lerp(p, q, k)
        }
    });
    *store = out;
    Ok(())
}

fn pixel_store(doc: &Document, layer: LayerId) -> EditResult<&TileStore> {
    let l = doc.layer(layer).ok_or(EditError::NoLayer(layer))?;
    l.pixels().ok_or(EditError::NotPixel(layer))
}

/// The layer's tile bounds (cheap), clipped to the selection's bounds.
fn affected_area(doc: &Document, layer: LayerId) -> Option<Rect> {
    let b = doc.layer(layer)?.pixels()?.bounds()?;
    Some(match selection_limit(doc.selection.as_ref(), doc.canvas()) {
        Some(l) => b.intersect(&l),
        None => b,
    })
}

/// Image ▸ Adjustments ▸ Shadows/Highlights.
pub struct ApplyShadowsHighlights {
    pub layer: LayerId,
    pub params: ShadowsHighlights,
}

impl Command for ApplyShadowsHighlights {
    fn label(&self) -> String {
        "Shadows/Highlights".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        affected_area(doc, self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        let store = pixel_store(doc, self.layer)?;
        if self.params.is_identity() {
            return Ok(());
        }
        let Some((b, done)) = adjust_more::shadows_highlights_layer(store, &self.params, canvas) else {
            return Ok(());
        };
        edit_pixels(doc, self.layer, |x, y, p| {
            if b.contains(x, y) {
                done.get((x - b.x) as u32, (y - b.y) as u32)
            } else {
                p
            }
        })
    }
}

/// Image ▸ Adjustments ▸ Equalize: spread the layer's gamma luminance
/// evenly over the tonal range, keeping each pixel's hue and chroma. With
/// a selection, the histogram comes from the selected pixels and, unless
/// `entire_layer` is set, only the selection changes (Photoshop's
/// "Equalize selected area only", its default).
pub struct Equalize {
    pub layer: LayerId,
    /// With a selection: equalise the whole layer based on the selection.
    pub entire_layer: bool,
}

impl Equalize {
    /// The equalisation curve this command would use.
    pub fn curve(&self, doc: &Document) -> EditResult<Option<[f32; EQ_LEVELS]>> {
        let store = pixel_store(doc, self.layer)?;
        let sel = doc.selection.as_ref();
        let canvas = doc.canvas();
        let mut hist = [0.0f64; EQ_LEVELS];
        for c in store.coords().collect::<Vec<_>>() {
            let Some(t) = store.tile(c) else { continue };
            // Only what is on the canvas counts, as in Photoshop.
            let r = c.rect().intersect(&canvas);
            if r.is_empty() {
                continue;
            }
            let px = t.pixels();
            let (ox, oy) = c.origin();
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let p = px[(y - oy) as usize * lumenply_tiles::TILE_SIZE + (x - ox) as usize];
                    let w = p.a * sel.map_or(1.0, |s| s.value(x, y));
                    histogram_add(&mut hist, p, w);
                }
            }
        }
        Ok(adjust_more::equalize_curve(&hist))
    }
}

impl Command for Equalize {
    fn label(&self) -> String {
        "Equalize".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        if self.entire_layer {
            return doc.layer(self.layer)?.pixels()?.bounds();
        }
        affected_area(doc, self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let Some(curve) = self.curve(doc)? else {
            return Ok(()); // a single tone: nothing to spread
        };
        if self.entire_layer {
            let sel = doc.selection.take();
            let r = edit_pixels(doc, self.layer, |_, _, p| adjust_more::equalize_pixel(p, &curve));
            doc.selection = sel;
            return r;
        }
        edit_pixels(doc, self.layer, |_, _, p| adjust_more::equalize_pixel(p, &curve))
    }
}

/// Image ▸ Adjustments ▸ Levels, Curves, Hue/Saturation, ...: any
/// adjustment-layer kind baked into the layer's pixels, exactly as the
/// adjustment layer would render it at 100 % Normal right above.
pub struct ApplyAdjustment {
    pub layer: LayerId,
    pub adjustment: lumenply_doc::Adjustment,
}

impl Command for ApplyAdjustment {
    fn label(&self) -> String {
        self.adjustment.name().into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        affected_area(doc, self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        pixel_store(doc, self.layer)?;
        let adj = self.adjustment.compile();
        edit_pixels(doc, self.layer, |_, _, p| {
            if p.a <= 0.0 {
                return p;
            }
            let s = p.to_straight();
            let o = adj.apply([s[0], s[1], s[2]]);
            Rgba::from_straight(o[0].max(0.0), o[1].max(0.0), o[2].max(0.0), s[3])
        })
    }
}

/// Image ▸ Adjustments ▸ Desaturate (Shift+Cmd+U): Hue/Saturation at −100.
pub struct Desaturate {
    pub layer: LayerId,
}

impl Command for Desaturate {
    fn label(&self) -> String {
        "Desaturate".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        affected_area(doc, self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        pixel_store(doc, self.layer)?;
        edit_pixels(doc, self.layer, |_, _, p| adjust_more::desaturate_pixel(p))
    }
}

/// Image ▸ Adjustments ▸ Replace Color: a Hue/Saturation change applied
/// through a soft colour-range mask of the layer's own pixels.
pub struct ApplyReplaceColor {
    pub layer: LayerId,
    pub params: ReplaceColor,
}

impl Command for ApplyReplaceColor {
    fn label(&self) -> String {
        "Replace Color".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        affected_area(doc, self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        pixel_store(doc, self.layer)?;
        let adj = self.params.adjustment().compile();
        let rc = &self.params;
        edit_pixels(doc, self.layer, |_, _, p| {
            let k = rc.weight(p);
            if k <= 0.0 {
                return p;
            }
            let s = p.to_straight();
            let o = adj.apply([s[0], s[1], s[2]]);
            lerp(p, Rgba::from_straight(o[0], o[1], o[2], s[3]), k)
        })
    }
}

/// Image ▸ Adjustments ▸ Match Color: move the layer's Lab statistics onto
/// a source's. The target's statistics come from the selection when
/// `target_from_selection` is set and there is one, else from the whole
/// layer; the change applies inside the selection.
pub struct ApplyMatchColor {
    pub layer: LayerId,
    pub params: MatchColor,
    pub target_from_selection: bool,
}

impl ApplyMatchColor {
    /// The target statistics the command matches from.
    pub fn target_stats(&self, doc: &Document) -> EditResult<Option<LabStats>> {
        let store = pixel_store(doc, self.layer)?;
        let canvas = doc.canvas();
        let sel = doc.selection.as_ref().filter(|_| self.target_from_selection);
        Ok(adjust_more::store_lab_stats(store, |x, y| {
            if !canvas.contains(x, y) {
                return 0.0;
            }
            sel.map_or(1.0, |s| s.value(x, y))
        }))
    }
}

impl Command for ApplyMatchColor {
    fn label(&self) -> String {
        "Match Color".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        affected_area(doc, self.layer)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let Some(target) = self.target_stats(doc)? else {
            return Ok(());
        };
        let m = self.params;
        edit_pixels(doc, self.layer, |_, _, p| m.apply(&target, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_doc::adjust::{srgb_decode, srgb_encode};
    use lumenply_doc::{LayerLocks, Mask};

    fn grey(g: f32) -> Rgba {
        let v = srgb_decode(g);
        Rgba::new(v, v, v, 1.0)
    }

    fn gamma(p: Rgba) -> [f32; 3] {
        let s = p.to_straight();
        [srgb_encode(s[0]), srgb_encode(s[1]), srgb_encode(s[2])]
    }

    fn close(a: f32, b: f32, t: f32) -> bool {
        (a - b).abs() <= t
    }

    /// A `w`×`h` document with one pixel layer painted by `f(x, y)`.
    fn doc_with(w: u32, h: u32, f: impl Fn(i32, i32) -> Rgba) -> (Editor, LayerId) {
        let mut doc = Document::new(w, h);
        let id = doc.add_pixel_layer("photo");
        let store = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                store.set_pixel(x, y, f(x, y));
            }
        }
        (Editor::new(doc), id)
    }

    fn px(ed: &Editor, id: LayerId, x: i32, y: i32) -> Rgba {
        ed.doc()
            .layer(id)
            .unwrap()
            .raster_store()
            .unwrap()
            .get_pixel(x, y)
    }

    fn select_rect(ed: &mut Editor, r: Rect) {
        ed.execute(&crate::commands::SetSelection {
            selection: Some(Selection::rect(r)),
        })
        .unwrap();
    }

    #[test]
    fn desaturate_acts_inside_the_selection_as_one_step() {
        let red = Rgba::new(1.0, 0.0, 0.0, 1.0);
        let (mut ed, id) = doc_with(20, 10, |_, _| red);
        select_rect(&mut ed, Rect::new(0, 0, 10, 10));
        let before = ed.history().len();
        ed.execute(&Desaturate { layer: id }).unwrap();
        assert_eq!(ed.history().len(), before + 1);
        assert_eq!(ed.history().last().copied(), Some("Desaturate"));
        // Inside: (max + min) / 2 = 0.5 in gamma; outside untouched.
        for v in gamma(px(&ed, id, 3, 3)) {
            assert!(close(v, 0.5, 1e-3), "{v}");
        }
        assert_eq!(px(&ed, id, 15, 3), red);
        ed.undo();
        assert_eq!(px(&ed, id, 3, 3), red);
    }

    #[test]
    fn equalize_uses_the_selections_histogram() {
        // Left half: columns alternate gamma 0.2 / 0.4; right half 0.6.
        let f = |x: i32, _y: i32| {
            if x >= 8 {
                grey(0.6)
            } else if x % 2 == 0 {
                grey(0.2)
            } else {
                grey(0.4)
            }
        };
        let (mut ed, id) = doc_with(16, 4, f);
        // Whole layer: levels 0.2 (16 px), 0.4 (16), 0.6 (32): cdf 16, 32,
        // 64; 0.2 -> 0, 0.4 -> 16/48 = 1/3, 0.6 -> 1.
        ed.execute(&Equalize {
            layer: id,
            entire_layer: false,
        })
        .unwrap();
        assert!(close(gamma(px(&ed, id, 0, 0))[0], 0.0, 1e-3));
        assert!(close(gamma(px(&ed, id, 1, 0))[0], 1.0 / 3.0, 1e-3));
        assert!(close(gamma(px(&ed, id, 12, 0))[0], 1.0, 1e-3));
        ed.undo();
        // Selected left half only: its own histogram (0.2 -> 0, 0.4 -> 1),
        // the right half untouched.
        select_rect(&mut ed, Rect::new(0, 0, 8, 4));
        ed.execute(&Equalize {
            layer: id,
            entire_layer: false,
        })
        .unwrap();
        assert!(close(gamma(px(&ed, id, 0, 0))[0], 0.0, 1e-3));
        assert!(close(gamma(px(&ed, id, 1, 0))[0], 1.0, 1e-3));
        assert!(close(gamma(px(&ed, id, 12, 0))[0], 0.6, 1e-3));
        ed.undo();
        // Entire layer based on the selection: 0.6 lies above the
        // selection's brightest level, so it maps to white too.
        ed.execute(&Equalize {
            layer: id,
            entire_layer: true,
        })
        .unwrap();
        assert!(close(gamma(px(&ed, id, 12, 0))[0], 1.0, 1e-3));
        assert!(ed.doc().selection.is_some(), "the selection stays");
    }

    #[test]
    fn shadows_highlights_lift_a_flat_dark_layer_and_respect_locks() {
        let (mut ed, id) = doc_with(48, 40, |_, _| grey(0.1));
        let params = ShadowsHighlights {
            shadows: ToneZone {
                amount: 1.0,
                tone: 0.5,
                radius: 6.0,
            },
            color: 0.0,
            ..Default::default()
        };
        ed.execute(&ApplyShadowsHighlights { layer: id, params }).unwrap();
        // Flat 0.1 everywhere (the canvas edge repeats, so the border is
        // no different): the shadow curve gives 0.29131.
        for (x, y) in [(0, 0), (24, 20), (47, 39)] {
            let g = gamma(px(&ed, id, x, y));
            assert!(close(g[0], 0.29131, 2e-3), "({x},{y}) {}", g[0]);
        }
        ed.undo();
        ed.execute(&crate::locks::SetLayerLocks {
            layer: id,
            locks: LayerLocks {
                pixels: true,
                ..LayerLocks::NONE
            },
        })
        .unwrap();
        assert!(ed.execute(&ApplyShadowsHighlights { layer: id, params }).is_err());
    }

    #[test]
    fn replace_color_shifts_only_matching_pixels() {
        let red = Rgba::new(0.8, 0.05, 0.05, 1.0);
        let blue = Rgba::new(0.05, 0.05, 0.8, 1.0);
        let (mut ed, id) = doc_with(10, 2, |x, _| if x < 5 { red } else { blue });
        ed.execute(&ApplyReplaceColor {
            layer: id,
            params: ReplaceColor {
                color: [0.8, 0.05, 0.05],
                fuzziness: 0.25,
                hue: 120.0,
                saturation: 0.0,
                lightness: 0.0,
            },
        })
        .unwrap();
        // Red rotated by +120° becomes green: gamma (0.906, 0.248, 0.248)
        // -> (0.248, 0.906, 0.248).
        let g = gamma(px(&ed, id, 1, 0));
        let r = gamma(red);
        assert!(
            close(g[1], r[0], 3e-3) && close(g[0], r[1], 3e-3) && close(g[2], r[2], 3e-3),
            "{g:?}"
        );
        let b = px(&ed, id, 7, 0);
        assert!(close(b.r, blue.r, 1e-4) && close(b.g, blue.g, 1e-4) && close(b.b, blue.b, 1e-4));
    }

    #[test]
    fn match_color_matches_the_source_statistics() {
        // L* 40 and 60 columns, matched to mean 50 / spread 20: 30 and 70.
        let col = |l: f32| {
            let c = adjust_more::lab_to_rgb([l, 0.0, 0.0]);
            Rgba::from_straight(c[0], c[1], c[2], 1.0)
        };
        let (mut ed, id) = doc_with(4, 4, |x, _| if x % 2 == 0 { col(40.0) } else { col(60.0) });
        let params = MatchColor {
            source: Some(LabStats {
                mean: [50.0, 0.0, 0.0],
                std: [20.0, 0.0, 0.0],
            }),
            ..Default::default()
        };
        ed.execute(&ApplyMatchColor {
            layer: id,
            params,
            target_from_selection: true,
        })
        .unwrap();
        let l = |p: Rgba| {
            let s = p.to_straight();
            adjust_more::rgb_to_lab([s[0], s[1], s[2]])[0]
        };
        assert!(close(l(px(&ed, id, 0, 0)), 30.0, 0.1));
        assert!(close(l(px(&ed, id, 1, 0)), 70.0, 0.1));
        assert_eq!(ed.history().last().copied(), Some("Match Color"));
    }

    #[test]
    fn apply_adjustment_bakes_like_the_layer_would_render() {
        let (mut ed, id) = doc_with(10, 4, |_, _| grey(0.2));
        select_rect(&mut ed, Rect::new(0, 0, 5, 4));
        ed.execute(&ApplyAdjustment {
            layer: id,
            adjustment: lumenply_doc::Adjustment::Invert,
        })
        .unwrap();
        assert_eq!(ed.history().last().copied(), Some("Invert"));
        // Inverted in gamma inside the selection: 0.2 -> 0.8.
        assert!(close(gamma(px(&ed, id, 2, 2))[0], 0.8, 1e-3));
        assert!(close(gamma(px(&ed, id, 7, 2))[0], 0.2, 1e-3));
        // Levels 0.2..0.6 stretch a 0.4 grey to 0.5 (gamma), everywhere.
        ed.undo();
        ed.execute(&crate::commands::SetSelection { selection: None })
            .unwrap();
        let (mut ed, id) = doc_with(4, 4, |_, _| grey(0.4));
        ed.execute(&ApplyAdjustment {
            layer: id,
            adjustment: lumenply_doc::Adjustment::Levels {
                in_black: 0.2,
                in_white: 0.6,
                gamma: 1.0,
                out_black: 0.0,
                out_white: 1.0,
                channels: Default::default(),
            },
        })
        .unwrap();
        assert!(
            close(gamma(px(&ed, id, 1, 1))[0], 0.5, 2e-3),
            "{:?}",
            gamma(px(&ed, id, 1, 1))
        );
    }

    #[test]
    fn adjustments_refuse_non_pixel_layers() {
        let mut doc = Document::new(8, 8);
        let id = doc.add_pixel_layer("a");
        let mut ed = Editor::new(doc);
        ed.execute(&crate::commands::AddAdjustmentLayer::new(
            lumenply_doc::Adjustment::Invert,
        ))
        .unwrap();
        let adj = ed.doc().layers().last().unwrap().id;
        assert!(ed.execute(&Desaturate { layer: adj }).is_err());
        assert!(ed.execute(&Desaturate { layer: id }).is_ok());
        let _ = Mask::hide_all();
    }
}
