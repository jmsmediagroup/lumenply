//! Eraser modes beyond the plain eraser: the Background eraser (erases
//! colours like the one under the brush centre) and the Magic eraser
//! (erases a flood-filled region). Re-exported from [`crate::commands`].

use lumenply_doc::adjust::srgb_encode;
use lumenply_doc::{Document, LayerId};
use lumenply_tiles::{Rect, Rgba};

use crate::commands::{
    dab_coverage, flood_region, interpolate_dabs, sampler, stroke_bounds, Brush, Dab, SampleSource,
    StrokePoint,
};
use crate::{Command, EditError, EditResult};

/// Gamma-encoded straight RGB and alpha.
fn encoded(p: Rgba) -> ([f32; 3], f32) {
    let [r, g, b, a] = p.to_straight();
    ([srgb_encode(r), srgb_encode(g), srgb_encode(b)], a)
}

/// How much of a pixel `d` away from the sampled colour is erased: all of
/// it up to three quarters of the tolerance, fading to none at the
/// tolerance (so edges against the kept colour come out soft).
pub fn tolerance_ramp(d: f32, tolerance: f32) -> f32 {
    let t = tolerance.clamp(0.0, 1.0);
    if d <= 0.75 * t {
        1.0
    } else if d <= t {
        (t - d) / (0.25 * t)
    } else {
        0.0
    }
}

/// Background eraser: within each dab, erase the pixels whose colour lies
/// within `tolerance` of the sampled colour (max channel difference of
/// gamma-encoded straight RGB, 0–1), protecting everything else. The
/// colour is sampled under the first dab's centre (`continuous` false) or
/// under every dab's centre; colours are read from the layer as it was
/// before the stroke. `contiguous` limits each dab to pixels connected to
/// its centre. `protect` (straight linear RGB, Photoshop's "Protect
/// foreground color") keeps every pixel within the tolerance of that
/// colour. Strength is `brush.color[3]`.
pub struct BackgroundErase {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
    pub tolerance: f32,
    pub continuous: bool,
    pub contiguous: bool,
    pub protect: Option<[f32; 3]>,
}

impl Command for BackgroundErase {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Background eraser".into()
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
        // The layer before the stroke (copy-on-write: no pixels copied).
        let before = store.clone();
        let strength = self.brush.color[3].clamp(0.0, 1.0);
        let dabs = interpolate_dabs(&self.brush, &self.points);
        let at = |p: &Dab| (p.x.floor() as i32, p.y.floor() as i32);
        let first = encoded(before.get_pixel(at(&dabs[0]).0, at(&dabs[0]).1));
        let protect = self
            .protect
            .map(|[r, g, b]| [srgb_encode(r), srgb_encode(g), srgb_encode(b)]);
        let dist = |c: [f32; 3], t: [f32; 3]| (0..3).map(|i| (c[i] - t[i]).abs()).fold(0.0, f32::max);
        for d in &dabs {
            let (target, ta) = if self.continuous {
                encoded(before.get_pixel(at(d).0, at(d).1))
            } else {
                first
            };
            if ta <= 0.0 {
                continue; // nothing under the centre to match
            }
            let amount = |x: i32, y: i32| {
                let (c, a) = encoded(before.get_pixel(x, y));
                if a <= 0.0 {
                    return 0.0;
                }
                if protect.is_some_and(|p| dist(c, p) <= self.tolerance.clamp(0.0, 1.0)) {
                    return 0.0;
                }
                tolerance_ramp(dist(c, target), self.tolerance)
            };
            // Contiguous: flood from the centre over erasable pixels of
            // the dab's box.
            let r = d.radius.ceil() as i32 + 1;
            let bx = Rect::new(at(d).0 - r, at(d).1 - r, (2 * r + 1) as u32, (2 * r + 1) as u32)
                .intersect(&canvas);
            let reach: Option<Vec<bool>> = (self.contiguous && !bx.is_empty()).then(|| {
                let w = bx.w as usize;
                let mut seen = vec![false; w * bx.h as usize];
                let (cx, cy) = at(d);
                let mut stack = Vec::new();
                if bx.contains(cx, cy) && amount(cx, cy) > 0.0 {
                    seen[(cy - bx.y) as usize * w + (cx - bx.x) as usize] = true;
                    stack.push((cx, cy));
                }
                while let Some((x, y)) = stack.pop() {
                    for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
                        if !bx.contains(nx, ny) {
                            continue;
                        }
                        let i = (ny - bx.y) as usize * w + (nx - bx.x) as usize;
                        if !seen[i] && amount(nx, ny) > 0.0 {
                            seen[i] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
                seen
            });
            dab_coverage(&self.brush, *d, canvas, sel.as_ref(), |px, py, cover| {
                if let Some(seen) = &reach {
                    if !bx.contains(px, py)
                        || !seen[(py - bx.y) as usize * bx.w as usize + (px - bx.x) as usize]
                    {
                        return;
                    }
                }
                let k = (amount(px, py) * cover * strength).clamp(0.0, 1.0);
                if k > 0.0 {
                    let dst = store.get_pixel(px, py);
                    store.set_pixel(px, py, dst.scale(1.0 - k));
                }
            });
        }
        store.prune_blank();
        Ok(())
    }
}

/// Magic eraser: erase the region around `(x, y)` whose colour lies within
/// `tolerance` of the clicked pixel (the magic wand's test: max channel
/// difference of straight linear RGBA), by `opacity`. One undo step.
pub struct MagicErase {
    pub layer: LayerId,
    pub x: i32,
    pub y: i32,
    pub tolerance: f32,
    pub contiguous: bool,
    pub sample: SampleSource,
    pub opacity: f32,
}

impl Command for MagicErase {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Magic eraser".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let canvas = doc.canvas();
        if !canvas.contains(self.x, self.y) {
            return Err(EditError::Invalid("click on the canvas".into()));
        }
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
        let opacity = self.opacity.clamp(0.0, 1.0);
        let area = region.bounds_within(canvas);
        for py in area.y..area.bottom() {
            for px in area.x..area.right() {
                let k = region.value(px, py) * opacity * sel.as_ref().map_or(1.0, |s| s.value(px, py));
                if k > 0.0 {
                    let dst = store.get_pixel(px, py);
                    store.set_pixel(px, py, dst.scale(1.0 - k));
                }
            }
        }
        store.prune_blank();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::BrushMode;
    use lumenply_doc::adjust::srgb_decode;

    fn g(r: f32, gg: f32, b: f32) -> Rgba {
        Rgba::from_straight(srgb_decode(r), srgb_decode(gg), srgb_decode(b), 1.0)
    }

    /// 32×32: green left of x = 16, red from there on; `paint` adjusts it.
    fn doc_with(paint: impl Fn(i32, i32) -> Option<Rgba>) -> (Document, LayerId) {
        let mut doc = Document::new(32, 32);
        let id = doc.add_pixel_layer("p");
        let store = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..32 {
            for x in 0..32 {
                let base = if x < 16 {
                    g(0.2, 0.6, 0.2)
                } else {
                    g(0.8, 0.1, 0.1)
                };
                store.set_pixel(x, y, paint(x, y).unwrap_or(base));
            }
        }
        (doc, id)
    }

    fn erase(
        id: LayerId,
        points: &[(f32, f32)],
        radius: f32,
        continuous: bool,
        contiguous: bool,
    ) -> BackgroundErase {
        BackgroundErase {
            layer: id,
            brush: Brush {
                radius,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, 1.0],
                spacing: 0.25,
                jitter: 0.0,
                mode: BrushMode::Erase,
                ..Brush::default()
            },
            points: points.iter().map(|&(x, y)| StrokePoint::new(x, y, 1.0)).collect(),
            tolerance: 0.1,
            continuous,
            contiguous,
            protect: None,
        }
    }

    fn alpha(doc: &Document, id: LayerId, x: i32, y: i32) -> f32 {
        doc.layer(id).unwrap().pixels().unwrap().get_pixel(x, y).a
    }

    #[test]
    fn the_ramp_erases_fully_then_fades_to_the_tolerance() {
        assert_eq!(tolerance_ramp(0.05, 0.1), 1.0);
        assert_eq!(tolerance_ramp(0.075, 0.1), 1.0);
        assert!((tolerance_ramp(0.09, 0.1) - 0.4).abs() < 1e-5);
        assert_eq!(tolerance_ramp(0.11, 0.1), 0.0);
        assert_eq!(
            tolerance_ramp(0.0, 0.0),
            1.0,
            "zero tolerance: exact matches only"
        );
        assert_eq!(tolerance_ramp(0.01, 0.0), 0.0);
    }

    #[test]
    fn the_background_eraser_erases_the_sampled_colour_only() {
        // Two near-greens: 0.05 off (erased), 0.09 off (60 % left).
        let (mut doc, id) = doc_with(|x, y| match (x, y) {
            (10, 12) => Some(g(0.25, 0.6, 0.2)),
            (12, 12) => Some(g(0.29, 0.6, 0.2)),
            _ => None,
        });
        erase(id, &[(12.0, 16.0)], 8.0, false, false)
            .apply(&mut doc)
            .unwrap();
        assert_eq!(alpha(&doc, id, 12, 16), 0.0, "green under the brush");
        assert_eq!(alpha(&doc, id, 10, 12), 0.0, "near-green");
        assert!(
            (alpha(&doc, id, 12, 12) - 0.6).abs() < 1e-3,
            "{}",
            alpha(&doc, id, 12, 12)
        );
        assert_eq!(alpha(&doc, id, 18, 16), 1.0, "red is protected");
        assert_eq!(alpha(&doc, id, 2, 16), 1.0, "outside the dab");
    }

    #[test]
    fn a_protected_colour_survives_the_background_eraser() {
        // (12, 12) is 0.07 off the sampled green: erased unless protected.
        // The protected colour is 0.07 from it but 0.14 from the green.
        let near = g(0.27, 0.6, 0.2);
        let (mut doc, id) = doc_with(|x, y| ((x, y) == (12, 12)).then_some(near));
        let mut e = erase(id, &[(12.0, 16.0)], 8.0, false, false);
        let [r, gg, b, _] = g(0.34, 0.6, 0.2).to_straight();
        e.protect = Some([r, gg, b]);
        e.apply(&mut doc).unwrap();
        assert_eq!(alpha(&doc, id, 12, 12), 1.0, "protected");
        assert_eq!(alpha(&doc, id, 12, 16), 0.0, "the sampled green still goes");
    }

    #[test]
    fn continuous_sampling_follows_the_brush_and_once_does_not() {
        let stroke = [(8.0, 16.0), (26.0, 16.0)];
        let (mut doc, id) = doc_with(|_, _| None);
        erase(id, &stroke, 3.0, false, false).apply(&mut doc).unwrap();
        assert_eq!(alpha(&doc, id, 10, 16), 0.0);
        assert_eq!(alpha(&doc, id, 24, 16), 1.0, "sampled once: green only");
        let (mut doc, id) = doc_with(|_, _| None);
        erase(id, &stroke, 3.0, true, false).apply(&mut doc).unwrap();
        assert_eq!(alpha(&doc, id, 10, 16), 0.0);
        assert_eq!(
            alpha(&doc, id, 24, 16),
            0.0,
            "continuous: the red under the brush too"
        );
    }

    #[test]
    fn contiguous_stops_at_a_protected_colour() {
        // A red stripe at x = 13..15 cuts off the green at x = 15 (left of
        // the red half) from the brush centre at x = 10.
        let paint = |x: i32, _y: i32| (13..15).contains(&x).then(|| g(0.8, 0.1, 0.1));
        let (mut doc, id) = doc_with(paint);
        erase(id, &[(10.0, 16.0)], 7.0, false, false)
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            alpha(&doc, id, 15, 16),
            0.0,
            "discontiguous reaches past the stripe"
        );
        let (mut doc, id) = doc_with(paint);
        erase(id, &[(10.0, 16.0)], 7.0, false, true)
            .apply(&mut doc)
            .unwrap();
        assert_eq!(alpha(&doc, id, 11, 16), 0.0);
        assert_eq!(alpha(&doc, id, 15, 16), 1.0, "contiguous stops at the stripe");
        assert_eq!(alpha(&doc, id, 13, 16), 1.0);
    }

    #[test]
    fn the_magic_eraser_clears_the_clicked_region() {
        let island =
            |x: i32, y: i32| ((24..28).contains(&x) && (4..8).contains(&y)).then(|| g(0.2, 0.6, 0.2));
        let magic = |id, contiguous, opacity| MagicErase {
            layer: id,
            x: 3,
            y: 3,
            tolerance: 0.05,
            contiguous,
            sample: SampleSource::Layer(id),
            opacity,
        };
        let (mut doc, id) = doc_with(island);
        magic(id, true, 1.0).apply(&mut doc).unwrap();
        assert_eq!(alpha(&doc, id, 0, 0), 0.0);
        assert_eq!(alpha(&doc, id, 15, 31), 0.0);
        assert_eq!(alpha(&doc, id, 16, 0), 1.0, "red stays");
        assert_eq!(alpha(&doc, id, 25, 5), 1.0, "the green island is not connected");
        let (mut doc, id) = doc_with(island);
        magic(id, false, 0.5).apply(&mut doc).unwrap();
        assert!(
            (alpha(&doc, id, 25, 5) - 0.5).abs() < 1e-6,
            "non-contiguous at half opacity"
        );
        assert!((alpha(&doc, id, 5, 5) - 0.5).abs() < 1e-6);
        assert_eq!(alpha(&doc, id, 20, 20), 1.0);
        let off = MagicErase {
            x: 40,
            ..magic(id, true, 1.0)
        };
        assert!(off.apply(&mut doc).is_err());
    }
}
