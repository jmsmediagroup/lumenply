//! The History brush ([`HistoryStroke`]), which paints pixels back from a
//! past state. Blur and Sharpen (`BrushMode::Blur` / `Sharpen` in a
//! `PaintStroke`) live with the other brush modes in
//! [`lumenply_render::paint`]. Re-exported from [`crate::commands`].

#[cfg(test)]
use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::{Document, LayerId};
#[cfg(test)]
use lumenply_render::paint::kernel_radius;
use lumenply_tiles::{Rect, Rgba, TileStore};

#[cfg(test)]
use crate::commands::BrushMode;
use crate::commands::{dab_coverage, interpolate_dabs, stroke_bounds, Brush, StrokePoint};
use crate::{Command, EditError, EditResult};

/// The history brush: paint a layer's pixels back from a past state.
/// `source` is the layer's pixels in that state (`None` when the layer did
/// not exist then, or the canvas has changed size since: pixels would no
/// longer line up). Each dab moves the layer toward the source by
/// strength (`brush.color[3]`) × coverage, premultiplied.
pub struct HistoryStroke {
    pub layer: LayerId,
    pub brush: Brush,
    pub points: Vec<StrokePoint>,
    pub source: Option<TileStore>,
}

impl Command for HistoryStroke {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "History brush".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(stroke_bounds(&self.brush, &self.points, doc.canvas()))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("stroke has no points".into()));
        }
        let source = self.source.as_ref().ok_or_else(|| {
            EditError::Invalid(
                "the history brush's source state lacks this layer or has another canvas size".into(),
            )
        })?;
        let canvas = doc.canvas();
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let strength = self.brush.color[3].clamp(0.0, 1.0);
        for d in interpolate_dabs(&self.brush, &self.points) {
            dab_coverage(&self.brush, d, canvas, sel.as_ref(), |px, py, cover| {
                let k = (strength * cover).clamp(0.0, 1.0);
                let (dst, src) = (store.get_pixel(px, py), source.get_pixel(px, py));
                store.set_pixel(
                    px,
                    py,
                    Rgba::new(
                        dst.r + (src.r - dst.r) * k,
                        dst.g + (src.g - dst.g) * k,
                        dst.b + (src.b - dst.b) * k,
                        dst.a + (src.a - dst.a) * k,
                    ),
                );
            });
        }
        store.prune_blank();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddPixelLayer, Fill, PaintStroke};
    use crate::Editor;

    /// 32×32, the left half `left`, the right half `right` (opaque).
    fn halves(left: Rgba, right: Rgba) -> (Document, LayerId) {
        let mut doc = Document::new(32, 32);
        let id = doc.add_pixel_layer("p");
        let store = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..32 {
            for x in 0..32 {
                store.set_pixel(x, y, if x < 16 { left } else { right });
            }
        }
        (doc, id)
    }

    fn dab(layer: LayerId, mode: BrushMode, strength: f32) -> PaintStroke {
        PaintStroke {
            layer,
            brush: Brush {
                radius: 3.0,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, strength],
                spacing: 1.0,
                jitter: 0.0,
                mode,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(16.0, 16.0, 1.0)],
        }
    }

    #[test]
    fn a_blur_dab_averages_its_three_by_three_neighbourhood() {
        let (mut doc, id) = halves(Rgba::new(0.0, 0.0, 0.0, 1.0), Rgba::new(1.0, 1.0, 1.0, 1.0));
        dab(id, BrushMode::Blur, 1.0).apply(&mut doc).unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        // Columns 14, 15 black and 16 white: 1/3. Columns 15, 16, 17: 2/3.
        let p = px.get_pixel(15, 16);
        assert!(
            (p.r - 1.0 / 3.0).abs() < 1e-6 && (p.a - 1.0).abs() < 1e-6,
            "{p:?}"
        );
        assert!((px.get_pixel(16, 16).g - 2.0 / 3.0).abs() < 1e-6);
        // A pixel whose box is all one colour stays put; so does one
        // outside the dab.
        assert_eq!(px.get_pixel(18, 16), Rgba::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(px.get_pixel(12, 16), Rgba::new(0.0, 0.0, 0.0, 1.0));

        // Half strength goes half way: 0 + (1/3 − 0) / 2; so does a
        // spacing of 0.5 (dabs twice as dense each do half as much), and
        // both together a quarter.
        for (strength, spacing, want) in [
            (0.5, 1.0, 1.0 / 6.0),
            (1.0, 0.5, 1.0 / 6.0),
            (0.5, 0.5, 1.0 / 12.0),
        ] {
            let (mut doc, id) = halves(Rgba::new(0.0, 0.0, 0.0, 1.0), Rgba::new(1.0, 1.0, 1.0, 1.0));
            let mut s = dab(id, BrushMode::Blur, strength);
            s.brush.spacing = spacing;
            s.apply(&mut doc).unwrap();
            let p = doc.layer(id).unwrap().pixels().unwrap().get_pixel(15, 16);
            assert!((p.b - want).abs() < 1e-6, "{strength} {spacing}: {p:?}");
        }
    }

    #[test]
    fn a_sharpen_dab_pushes_an_edge_apart_in_gamma() {
        let grey = |v: f32| {
            let l = srgb_decode(v);
            Rgba::new(l, l, l, 1.0)
        };
        let (mut doc, id) = halves(grey(0.25), grey(0.75));
        assert_eq!(dab(id, BrushMode::Sharpen, 1.0).label(), "Sharpen");
        dab(id, BrushMode::Sharpen, 1.0).apply(&mut doc).unwrap();
        let px = doc.layer(id).unwrap().pixels().unwrap();
        let enc = |p: Rgba| srgb_encode(p.to_straight()[0]);
        // Box means 5/12 and 7/12; 0.25 − 0.6·(1/6) = 0.15, 0.75 + 0.1 = 0.85.
        assert!((enc(px.get_pixel(15, 16)) - 0.15).abs() < 1e-4);
        assert!((enc(px.get_pixel(16, 16)) - 0.85).abs() < 1e-4);
        assert!((enc(px.get_pixel(13, 16)) - 0.25).abs() < 1e-5, "flat areas stay");
    }

    #[test]
    fn big_brushes_blur_five_by_five() {
        let mut b = Brush {
            radius: 11.9,
            ..Brush::default()
        };
        assert_eq!(kernel_radius(&b), 1);
        b.radius = 12.0;
        assert_eq!(kernel_radius(&b), 2);
        let (mut doc, id) = halves(Rgba::new(0.0, 0.0, 0.0, 1.0), Rgba::new(1.0, 1.0, 1.0, 1.0));
        let mut s = dab(id, BrushMode::Blur, 1.0);
        s.brush.radius = 12.0;
        s.apply(&mut doc).unwrap();
        // Columns 13..=17: two white of five.
        let p = doc.layer(id).unwrap().pixels().unwrap().get_pixel(15, 16);
        assert!((p.r - 0.4).abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn a_paint_stroke_in_history_mode_is_refused() {
        let (mut doc, id) = halves(Rgba::TRANSPARENT, Rgba::TRANSPARENT);
        assert!(dab(id, BrushMode::History, 1.0).apply(&mut doc).is_err());
    }

    #[test]
    fn the_history_brush_paints_a_past_state_back() {
        let mut ed = Editor::new(Document::new(32, 32));
        ed.execute(&AddPixelLayer::new("L")).unwrap();
        let id = ed.doc().layers()[0].id;
        ed.execute(&Fill {
            layer: id,
            color: [1.0, 0.0, 0.0, 1.0],
        })
        .unwrap();
        ed.execute(&Fill {
            layer: id,
            color: [0.0, 0.0, 1.0, 1.0],
        })
        .unwrap();
        // States: 0 opened, 1 layer added, 2 red, 3 blue (current).
        assert!(ed.state(0).unwrap().layers().is_empty());
        assert!(std::ptr::eq(ed.state(3).unwrap(), ed.doc()));
        assert!(ed.state(4).is_none());
        let red = ed.state(2).unwrap().layer(id).unwrap().pixels().cloned();
        let stroke = |strength: f32, source: Option<TileStore>| HistoryStroke {
            layer: id,
            brush: Brush {
                radius: 4.0,
                hardness: 1.0,
                color: [0.0, 0.0, 0.0, strength],
                spacing: 0.2,
                jitter: 0.0,
                mode: BrushMode::History,
                ..Brush::default()
            },
            points: vec![StrokePoint::new(16.0, 16.0, 1.0)],
            source,
        };
        ed.execute(&stroke(0.5, red.clone())).unwrap();
        let p = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(16, 16);
        assert!((p.r - 0.5).abs() < 1e-4 && (p.b - 0.5).abs() < 1e-4 && (p.a - 1.0).abs() < 1e-4);
        ed.execute(&stroke(1.0, red)).unwrap();
        let px = ed.doc().layer(id).unwrap().pixels().unwrap();
        let p = px.get_pixel(16, 16);
        assert!((p.r - 1.0).abs() < 1e-4 && p.b.abs() < 1e-4, "{p:?}");
        assert!(
            (px.get_pixel(2, 2).to_straight()[2] - 1.0).abs() < 1e-4,
            "outside the dab: still blue"
        );
        assert_eq!(ed.history().last().copied(), Some("History brush"));

        // Redo states are reachable too.
        ed.undo();
        assert!(ed.state(5).is_some() && ed.state(6).is_none());
        // A layer that didn't exist in the source state can't be painted from it.
        assert!(ed.execute(&stroke(1.0, None)).is_err());
    }
}
