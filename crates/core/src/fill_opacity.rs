//! Photoshop's "Fill" (fill opacity): fades a layer's own content but not
//! its layer effects. Slider drags run through `execute_coalescing`.

use lumenply_doc::{Document, LayerId};

use crate::{Command, EditError, EditResult};

pub struct SetFillOpacity {
    pub layer: LayerId,
    /// 0.0 to 1.0; anything else is clamped (NaN reads as 100%).
    pub fill: f32,
}

impl Command for SetFillOpacity {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Set fill".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.fill_opacity = if self.fill.is_nan() {
            1.0
        } else {
            self.fill.clamp(0.0, 1.0)
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use lumenply_doc::{Layer, LayerEffects, StrokeAlign, StrokeFx};
    use lumenply_tiles::{Rgba, TileStore};

    use super::*;
    use crate::Editor;

    /// An 8×8 document: a white background and an opaque red square
    /// (2..6) with a 1-px blue outside stroke.
    fn doc() -> (Editor, LayerId) {
        let mut d = Document::new(8, 8);
        let mut bg = Layer::pixel(d.alloc_id(), "bg");
        let white = bg.pixels_mut().expect("pixel");
        for y in 0..8 {
            for x in 0..8 {
                white.set_pixel(x, y, Rgba::new(1.0, 1.0, 1.0, 1.0));
            }
        }
        d.add_layer(bg);
        let id = d.alloc_id();
        let mut sq = Layer::pixel(id, "square");
        let mut store = TileStore::new();
        for y in 2..6 {
            for x in 2..6 {
                store.set_pixel(x, y, Rgba::new(1.0, 0.0, 0.0, 1.0));
            }
        }
        *sq.pixels_mut().expect("pixel") = store;
        sq.effects = LayerEffects {
            stroke: Some(StrokeFx {
                size: 1.0,
                color: [0.0, 0.0, 1.0],
                opacity: 1.0,
                position: StrokeAlign::Outside,
                ..StrokeFx::default()
            }),
            ..LayerEffects::default()
        };
        d.add_layer(sq);
        (Editor::new(d), id)
    }

    fn px(ed: &Editor, x: u32, y: u32) -> Rgba {
        lumenply_render::composite_raster(ed.doc()).get(x, y)
    }

    #[test]
    fn fill_fades_the_content_but_not_its_effects() {
        let (mut ed, id) = doc();
        ed.execute(&SetFillOpacity {
            layer: id,
            fill: 0.25,
        })
        .unwrap();
        assert_eq!(ed.doc().layer(id).unwrap().fill_opacity, 0.25);
        // Inside: 25% red over white (linear light): r = 1, g = b = 0.75.
        let inside = px(&ed, 3, 3);
        assert!((inside.r - 1.0).abs() < 1e-3, "{inside:?}");
        assert!(
            (inside.g - 0.75).abs() < 1e-3 && (inside.b - 0.75).abs() < 1e-3,
            "{inside:?}"
        );
        // The stroke ring outside the square stays fully blue.
        let ring = px(&ed, 1, 3);
        assert!(
            ring.r.abs() < 1e-3 && ring.g.abs() < 1e-3 && (ring.b - 1.0).abs() < 1e-3,
            "{ring:?}"
        );
        // Opacity, unlike fill, fades both.
        ed.execute(&SetFillOpacity { layer: id, fill: 1.0 }).unwrap();
        ed.execute(&crate::commands::SetOpacity {
            layer: id,
            opacity: 0.25,
        })
        .unwrap();
        let ring = px(&ed, 1, 3);
        assert!(
            (ring.r - 0.75).abs() < 1e-3 && (ring.b - 1.0).abs() < 1e-3,
            "{ring:?}"
        );
    }

    #[test]
    fn zero_fill_shows_a_knocked_out_drop_shadow_only_outside() {
        let (mut ed, id) = doc();
        let shadow = |knockout| lumenply_doc::ShadowFx {
            dx: 1.0,
            dy: 0.0,
            blur: 0.0,
            color: [0.0, 0.0, 0.0],
            opacity: 1.0,
            knockout,
            ..lumenply_doc::ShadowFx::default()
        };
        for knockout in [true, false] {
            ed.execute(&crate::commands::SetLayerEffects {
                layer: id,
                effects: LayerEffects {
                    drop_shadow: Some(shadow(knockout)),
                    ..LayerEffects::default()
                },
            })
            .unwrap();
            ed.execute(&SetFillOpacity { layer: id, fill: 0.0 }).unwrap();
            // x = 6 is just right of the square: shadowed either way (the
            // minimum blur still softens it a little).
            assert!(px(&ed, 6, 3).r < 0.8, "{:?}", px(&ed, 6, 3));
            // Under the (invisible) square the shadow shows only without
            // knockout: exactly white with it, dark without.
            let under = px(&ed, 4, 3);
            if knockout {
                assert!(
                    (under.r - 1.0).abs() < 1e-5 && (under.g - 1.0).abs() < 1e-5,
                    "{under:?}"
                );
            } else {
                assert!(under.r < 0.5 && under.g < 0.5, "{under:?}");
            }
        }
    }

    #[test]
    fn fill_is_clamped_and_slider_drags_undo_in_one_step() {
        let (mut ed, id) = doc();
        let steps = ed.history().len();
        for v in [0.9, 0.5, -3.0] {
            ed.execute_coalescing(&SetFillOpacity { layer: id, fill: v }, "fill")
                .unwrap();
        }
        ed.end_coalescing();
        assert_eq!(ed.doc().layer(id).unwrap().fill_opacity, 0.0);
        assert_eq!(ed.history().len(), steps + 1);
        assert_eq!(ed.history().last().copied(), Some("Set fill"));
        ed.undo();
        assert_eq!(ed.doc().layer(id).unwrap().fill_opacity, 1.0);
        ed.execute(&SetFillOpacity {
            layer: id,
            fill: f32::NAN,
        })
        .unwrap();
        assert_eq!(ed.doc().layer(id).unwrap().fill_opacity, 1.0);
        assert!(ed
            .execute(&SetFillOpacity {
                layer: 999,
                fill: 0.5
            })
            .is_err());
    }
}
