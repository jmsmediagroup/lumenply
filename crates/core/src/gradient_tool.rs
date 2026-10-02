//! The Gradient tool's commands: draw a multi-stop gradient along a
//! dragged line onto a pixel layer or a layer mask ([`DrawGradient`]), or
//! turn the drag into an editable gradient fill layer
//! ([`GradientFillLayer`], built on [`AddFillLayer`]).
//!
//! The pixel work lives in `lumenply_render::gradient_draw` (geometry,
//! dither, blend). Layer locks are enforced by the editor like for every
//! other pixel edit.

use lumenply_doc::{Document, Fill, GradientStyle, LayerId};
use lumenply_tiles::Rect;

pub use lumenply_render::gradient_draw::{ramp_position, GradientPaint};

use crate::fill_cmds::AddFillLayer;
use crate::{Command, EditError, EditResult};

/// What a gradient drag paints on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GradientTarget {
    /// The layer's pixels.
    #[default]
    Pixels,
    /// The layer's mask (grey levels; any layer kind with a mask).
    Mask,
}

/// Draw a gradient over the selection (or the whole canvas), blended by
/// its coverage, the paint's opacity and blend mode.
pub struct DrawGradient {
    pub layer: LayerId,
    pub target: GradientTarget,
    pub paint: GradientPaint,
}

/// The canvas area a gradient changes: the selection's bounds, or the
/// canvas.
fn area(doc: &Document) -> Rect {
    let canvas = doc.canvas();
    doc.selection.as_ref().map_or(canvas, |s| s.bounds_within(canvas))
}

impl Command for DrawGradient {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        match self.target {
            GradientTarget::Pixels => "Gradient".into(),
            GradientTarget::Mask => "Gradient on mask".into(),
        }
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(area(doc))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.paint.length() < 1e-3 {
            return Err(EditError::Invalid("drag a line to draw a gradient".into()));
        }
        let area = area(doc);
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        match self.target {
            GradientTarget::Pixels => {
                let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
                lumenply_render::gradient_draw::paint_pixels(store, area, sel.as_ref(), &self.paint);
            }
            GradientTarget::Mask => {
                let mask = layer
                    .mask
                    .as_mut()
                    .ok_or_else(|| EditError::Invalid(format!("layer {} has no mask", self.layer)))?;
                lumenply_render::gradient_draw::paint_mask(mask, area, sel.as_ref(), &self.paint);
            }
        }
        Ok(())
    }
}

/// The gradient fill a drag makes: the same ramp, style and direction as
/// [`DrawGradient`] would paint, expressed in the fill layer's
/// canvas-relative settings (angle, scale against the canvas span along
/// the angle, centre offset). Stop opacities are dropped when the paint's
/// Transparency is off; fills have no dither.
pub fn fill_for_drag(paint: &GradientPaint, canvas: Rect) -> Fill {
    let (dx, dy) = (paint.end.0 - paint.start.0, paint.end.1 - paint.start.1);
    let len = paint.length().max(1e-3);
    let a = (-dy).atan2(dx);
    let (w, h) = (canvas.w.max(1) as f32, canvas.h.max(1) as f32);
    // The fill's full ramp length at 100% (see `Fill::sampler`).
    let span = ((w * a.cos()).abs() + (h * a.sin()).abs()).max(1.0);
    // Linear ramps run through their centre; the others start at it.
    let (centre, ramp) = match paint.style {
        GradientStyle::Linear => (
            (
                (paint.start.0 + paint.end.0) / 2.0,
                (paint.start.1 + paint.end.1) / 2.0,
            ),
            len,
        ),
        _ => (paint.start, 2.0 * len),
    };
    let mut gradient = paint.gradient.clone();
    if !paint.transparency {
        for s in &mut gradient.stops {
            s.alpha = 1.0;
        }
    }
    Fill::Gradient {
        gradient,
        style: paint.style,
        angle: a.to_degrees(),
        scale: if paint.style == GradientStyle::Angle {
            1.0
        } else {
            ramp / span
        },
        reverse: paint.reverse,
        offset: [
            (centre.0 - (canvas.x as f32 + w / 2.0)) / w,
            (centre.1 - (canvas.y as f32 + h / 2.0)) / h,
        ],
    }
}

/// The Gradient tool in fill-layer mode: the drag becomes a gradient fill
/// layer above `above` (masked by the selection), carrying the paint's
/// blend mode and opacity. One undo step.
pub struct GradientFillLayer {
    pub paint: GradientPaint,
    pub above: Option<LayerId>,
}

impl Command for GradientFillLayer {
    fn label(&self) -> String {
        "Add Gradient Fill layer".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(doc.canvas())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.paint.length() < 1e-3 {
            return Err(EditError::Invalid("drag a line to draw a gradient".into()));
        }
        let id = doc.next_id();
        AddFillLayer {
            fill: fill_for_drag(&self.paint, doc.canvas()),
            above: self.above,
            mask_selection: true,
        }
        .apply(doc)?;
        let l = doc
            .layer_mut(id)
            .ok_or_else(|| EditError::Invalid("the fill layer went missing".into()))?;
        l.blend = self.paint.blend;
        l.opacity = self.paint.opacity.clamp(0.0, 1.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_doc::adjust::srgb_encode;
    use lumenply_doc::{BlendMode, Gradient, GradientStop, LayerLocks, Mask, Selection};
    use lumenply_tiles::Rgba;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    /// A `w × h` document with one empty pixel layer.
    fn doc(w: u32, h: u32) -> (Document, LayerId) {
        let mut d = Document::new(w, h);
        let id = d.add_pixel_layer("p");
        (d, id)
    }

    fn draw(doc: &mut Document, id: LayerId, paint: GradientPaint) {
        DrawGradient {
            layer: id,
            target: GradientTarget::Pixels,
            paint,
        }
        .apply(doc)
        .unwrap();
    }

    /// The gamma-encoded red channel and the alpha of a pixel.
    fn enc(doc: &Document, id: LayerId, x: i32, y: i32) -> (f32, f32) {
        let p = doc.layer(id).unwrap().pixels().unwrap().get_pixel(x, y);
        let s = p.to_straight();
        (srgb_encode(s[0]), s[3])
    }

    fn bw(style: GradientStyle, start: (f32, f32), end: (f32, f32)) -> GradientPaint {
        GradientPaint::new(Gradient::default(), style, start, end)
    }

    #[test]
    fn linear_and_reverse_follow_the_drag() {
        let (mut d, id) = doc(200, 10);
        // Black → white from x = 0 to x = 100: pixel 49's centre is at
        // t = 0.495, sRGB 0.495 (the ramp runs on gamma values); past the
        // end it stays white.
        draw(&mut d, id, bw(GradientStyle::Linear, (0.0, 5.0), (100.0, 5.0)));
        assert!(close(enc(&d, id, 0, 3).0, 0.005, 1e-3), "{:?}", enc(&d, id, 0, 3));
        assert!(
            close(enc(&d, id, 49, 3).0, 0.495, 1e-3),
            "{:?}",
            enc(&d, id, 49, 3)
        );
        assert!(close(enc(&d, id, 99, 3).0, 0.995, 1e-3));
        assert!(close(enc(&d, id, 150, 3).0, 1.0, 1e-4));
        assert_eq!(enc(&d, id, 49, 3).1, 1.0, "opaque");

        let (mut d, id) = doc(200, 10);
        let mut p = bw(GradientStyle::Linear, (0.0, 5.0), (100.0, 5.0));
        p.reverse = true;
        draw(&mut d, id, p);
        assert!(close(enc(&d, id, 0, 3).0, 0.995, 1e-3));
        assert!(close(enc(&d, id, 74, 3).0, 0.255, 1e-3));
        assert!(close(enc(&d, id, 190, 3).0, 0.0, 1e-4));
    }

    #[test]
    fn radial_angle_reflected_and_diamond_place_known_points() {
        // Every drag starts on pixel (50, 50)'s centre and runs 40 px right
        // (20 px for reflected and diamond).
        let s = (50.5, 50.5);
        let (mut d, id) = doc(100, 100);
        draw(&mut d, id, bw(GradientStyle::Radial, s, (90.5, 50.5)));
        assert!(close(enc(&d, id, 50, 50).0, 0.0, 1e-4));
        assert!(close(enc(&d, id, 70, 50).0, 0.5, 1e-3), "20 px right");
        assert!(close(enc(&d, id, 50, 30).0, 0.5, 1e-3), "20 px up: a circle");
        assert!(close(enc(&d, id, 80, 80).0, 1.0, 1e-4), "42 px out: past the end");

        let (mut d, id) = doc(100, 100);
        draw(&mut d, id, bw(GradientStyle::Angle, s, (90.5, 50.5)));
        // Counter-clockwise from the drag direction, as on screen.
        assert!(close(enc(&d, id, 70, 50).0, 0.0, 1e-3), "along the drag");
        assert!(close(enc(&d, id, 50, 30).0, 0.25, 1e-3), "a quarter turn up");
        assert!(close(enc(&d, id, 30, 50).0, 0.5, 1e-3), "half a turn");
        assert!(close(enc(&d, id, 50, 70).0, 0.75, 1e-3), "three quarters");
        assert!(close(enc(&d, id, 70, 30).0, 0.125, 1e-3), "45° up-right");

        let (mut d, id) = doc(100, 100);
        draw(&mut d, id, bw(GradientStyle::Reflected, s, (70.5, 50.5)));
        assert!(close(enc(&d, id, 60, 10).0, 0.5, 1e-3));
        assert!(
            close(enc(&d, id, 40, 90).0, 0.5, 1e-3),
            "mirrored behind the start"
        );
        assert!(close(enc(&d, id, 50, 0).0, 0.0, 1e-4));
        assert!(close(enc(&d, id, 20, 0).0, 1.0, 1e-4));

        let (mut d, id) = doc(100, 100);
        draw(&mut d, id, bw(GradientStyle::Diamond, s, (70.5, 50.5)));
        assert!(close(enc(&d, id, 60, 50).0, 0.5, 1e-3));
        assert!(close(enc(&d, id, 55, 55).0, 0.5, 1e-3), "|5| + |5| = 10 of 20");
        assert!(close(enc(&d, id, 50, 40).0, 0.5, 1e-3));
        assert!(close(enc(&d, id, 52, 45).0, 0.35, 1e-3), "|2| + |5| = 7 of 20");
        assert!(close(enc(&d, id, 65, 60).0, 1.0, 1e-4), "outside the diamond");
    }

    #[test]
    fn stops_opacity_transparency_and_blend_modes() {
        // Red → green → blue: a quarter of the way is sRGB (0.5, 0.5, 0).
        let rgb = Gradient {
            stops: vec![
                GradientStop::srgb8(0.0, [255, 0, 0]),
                GradientStop::srgb8(0.5, [0, 255, 0]),
                GradientStop::srgb8(1.0, [0, 0, 255]),
            ],
        };
        let (mut d, id) = doc(100, 4);
        draw(
            &mut d,
            id,
            GradientPaint::new(rgb, GradientStyle::Linear, (0.0, 0.0), (100.0, 0.0)),
        );
        let s = d
            .layer(id)
            .unwrap()
            .pixels()
            .unwrap()
            .get_pixel(24, 1)
            .to_straight();
        let e = [s[0], s[1], s[2]].map(srgb_encode);
        assert!(
            close(e[0], 0.51, 2e-3) && close(e[1], 0.49, 2e-3) && e[2] < 1e-3,
            "{e:?}"
        );

        // White → transparent white: half-way is half opaque; with
        // Transparency off the stops paint opaque.
        let fade = Gradient {
            stops: vec![
                GradientStop::new(0.0, [1.0; 3]),
                GradientStop {
                    alpha: 0.0,
                    ..GradientStop::new(1.0, [1.0; 3])
                },
            ],
        };
        let mut p = GradientPaint::new(fade, GradientStyle::Linear, (0.0, 0.0), (100.0, 0.0));
        let (mut d, id) = doc(100, 4);
        draw(&mut d, id, p.clone());
        assert!(
            close(enc(&d, id, 49, 1).1, 0.505, 1e-3),
            "{:?}",
            enc(&d, id, 49, 1)
        );
        p.transparency = false;
        let (mut d, id) = doc(100, 4);
        draw(&mut d, id, p);
        assert_eq!(enc(&d, id, 49, 1).1, 1.0);

        // Over a mid-grey layer (linear 0.5): 50% white gives 0.75;
        // Multiply by sRGB-grey (linear 0.2140) gives 0.1070.
        let grey = |d: &mut Document, id: LayerId| {
            let px = d.layer_mut(id).unwrap().pixels_mut().unwrap();
            for x in 0..10 {
                px.set_pixel(x, 0, Rgba::new(0.5, 0.5, 0.5, 1.0));
            }
        };
        let flat = |c: [f32; 3]| Gradient::two(c, c);
        let (mut d, id) = doc(10, 1);
        grey(&mut d, id);
        let mut p = GradientPaint::new(flat([1.0; 3]), GradientStyle::Linear, (0.0, 0.0), (10.0, 0.0));
        p.opacity = 0.5;
        draw(&mut d, id, p);
        let v = d.layer(id).unwrap().pixels().unwrap().get_pixel(3, 0);
        assert!(close(v.r, 0.75, 1e-4) && v.a == 1.0, "{v:?}");
        let (mut d, id) = doc(10, 1);
        grey(&mut d, id);
        let g = lumenply_doc::adjust::srgb_decode(0.5);
        let mut p = GradientPaint::new(flat([g; 3]), GradientStyle::Linear, (0.0, 0.0), (10.0, 0.0));
        p.blend = BlendMode::Multiply;
        draw(&mut d, id, p);
        let v = d.layer(id).unwrap().pixels().unwrap().get_pixel(3, 0);
        assert!(close(v.r, 0.5 * 0.2140, 1e-4), "{v:?}");
    }

    #[test]
    fn the_selection_limits_and_softens_the_gradient() {
        let (mut d, id) = doc(100, 10);
        let mut sel = Selection::rect(Rect::new(0, 0, 50, 10));
        sel.coverage.set_value(40, 5, 0.25);
        d.selection = Some(sel);
        let white = Gradient::two([1.0; 3], [1.0; 3]);
        let paint = GradientPaint::new(white, GradientStyle::Linear, (0.0, 0.0), (100.0, 0.0));
        let cmd = DrawGradient {
            layer: id,
            target: GradientTarget::Pixels,
            paint,
        };
        // The selection's (tile-granular) bounds, clipped to the canvas.
        assert_eq!(cmd.affected(&d), Some(Rect::new(0, 0, 100, 10)));
        cmd.apply(&mut d).unwrap();
        let px = d.layer(id).unwrap().pixels().unwrap();
        assert_eq!(px.get_pixel(10, 5).a, 1.0, "inside");
        assert_eq!(px.get_pixel(60, 5).a, 0.0, "outside untouched");
        assert!(close(px.get_pixel(40, 5).a, 0.25, 1e-6), "blended by coverage");
    }

    #[test]
    fn dither_breaks_bands_without_bias_and_repeats_exactly() {
        // A shallow ramp: sRGB 0.40 → 0.42 over 512 px is five 8-bit
        // levels, so plain rounding leaves ~100 px bands.
        let c = |v: f32| [lumenply_doc::adjust::srgb_decode(v); 3];
        let ramp = Gradient::two(c(0.40), c(0.42));
        let run = |dither: bool| {
            let (mut d, id) = doc(512, 64);
            let mut p = GradientPaint::new(ramp.clone(), GradientStyle::Linear, (0.0, 0.0), (512.0, 0.0));
            p.dither = dither;
            draw(&mut d, id, p);
            d.layer(id).unwrap().pixels().unwrap().clone()
        };
        let (plain, dith) = (run(false), run(true));
        let byte = |s: &lumenply_tiles::TileStore, x: i32, y: i32| {
            (srgb_encode(s.get_pixel(x, y).r) * 255.0).round()
        };
        let mut worst_plain = 0.0f32;
        let mut worst_dith = 0.0f32;
        for x in (0..512).step_by(7) {
            let ideal = (0.40 + 0.02 * (x as f32 + 0.5) / 512.0) * 255.0;
            let mean = |s| (0..64).map(|y| byte(s, x, y)).sum::<f32>() / 64.0;
            worst_plain = worst_plain.max((mean(&plain) - ideal).abs());
            worst_dith = worst_dith.max((mean(&dith) - ideal).abs());
            for y in 0..64 {
                assert!(
                    (byte(&dith, x, y) - byte(&plain, x, y)).abs() <= 1.0,
                    "within one step"
                );
            }
        }
        assert!(
            worst_plain > 0.45,
            "plain rounding is off by up to half a step: {worst_plain}"
        );
        assert!(
            worst_dith < 0.12,
            "dithered columns average to the true value: {worst_dith}"
        );
        // Deterministic: the same drag gives the same pixels.
        let again = run(true);
        assert!((0..512).all(|x| again.get_pixel(x, 9) == dith.get_pixel(x, 9)));
    }

    #[test]
    fn masks_take_the_ramp_as_grey_levels() {
        let (mut d, id) = doc(100, 4);
        d.layer_mut(id).unwrap().mask = Some(Mask::reveal_all());
        let cmd = DrawGradient {
            layer: id,
            target: GradientTarget::Mask,
            paint: bw(GradientStyle::Linear, (0.0, 0.0), (100.0, 0.0)),
        };
        assert_eq!(cmd.label(), "Gradient on mask");
        cmd.apply(&mut d).unwrap();
        let m = d.layer(id).unwrap().mask.as_ref().unwrap();
        // Black → white hides linearly: the grey level is the encoded value.
        assert!(close(m.value(0, 1), 0.005, 1e-3));
        assert!(close(m.value(49, 1), 0.495, 1e-3), "{}", m.value(49, 1));
        assert!(close(m.value(99, 1), 0.995, 1e-3));
        assert!(
            d.layer(id).unwrap().pixels().unwrap().is_empty(),
            "pixels untouched"
        );
        // No mask: refused; a pixel target on a non-pixel layer too.
        let (mut d, id) = doc(10, 10);
        let mut cmd = DrawGradient {
            layer: id,
            target: GradientTarget::Mask,
            paint: bw(GradientStyle::Linear, (0.0, 0.0), (10.0, 0.0)),
        };
        assert!(cmd.apply(&mut d).is_err());
        cmd.target = GradientTarget::Pixels;
        cmd.paint.end = cmd.paint.start;
        assert!(cmd.apply(&mut d).is_err(), "a zero-length drag draws nothing");
    }

    #[test]
    fn layer_locks_hold_through_the_editor() {
        let (mut d, id) = doc(20, 4);
        d.layer_mut(id).unwrap().locks = LayerLocks {
            pixels: true,
            ..LayerLocks::NONE
        };
        let mut ed = Editor::new(d);
        let cmd = DrawGradient {
            layer: id,
            target: GradientTarget::Pixels,
            paint: bw(GradientStyle::Linear, (0.0, 0.0), (20.0, 0.0)),
        };
        assert!(ed.execute(&cmd).is_err(), "pixels locked");
        // Transparency locked: an empty layer stays empty.
        let (mut d, id) = doc(20, 4);
        d.layer_mut(id).unwrap().locks = LayerLocks {
            transparency: true,
            ..LayerLocks::NONE
        };
        let mut ed = Editor::new(d);
        ed.execute(&DrawGradient { layer: id, ..cmd }).unwrap();
        assert_eq!(
            ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(5, 1).a,
            0.0
        );
    }

    #[test]
    fn a_fill_layer_from_a_drag_matches_the_painted_gradient() {
        let canvas = Rect::new(0, 0, 120, 80);
        for (style, start, end) in [
            (GradientStyle::Linear, (10.0, 20.0), (90.0, 60.0)),
            (GradientStyle::Radial, (60.0, 40.0), (60.0, 10.0)),
            (GradientStyle::Angle, (30.0, 50.0), (80.0, 20.0)),
            (GradientStyle::Reflected, (60.0, 40.0), (100.0, 40.0)),
            (GradientStyle::Diamond, (50.0, 30.0), (70.0, 45.0)),
        ] {
            let mut paint = bw(style, start, end);
            paint.reverse = style == GradientStyle::Diamond;
            let (mut d, id) = doc(canvas.w, canvas.h);
            draw(&mut d, id, paint.clone());
            paint.opacity = 0.6;
            paint.blend = BlendMode::Screen;
            GradientFillLayer { paint, above: None }.apply(&mut d).unwrap();
            let fill = d.layers().last().unwrap();
            assert_eq!((fill.blend, fill.opacity), (BlendMode::Screen, 0.6));
            let cache = fill.fill_layer().unwrap().cache.as_ref().unwrap();
            let painted = d.layer(id).unwrap().pixels().unwrap();
            for (x, y) in [(5, 5), (33, 41), (60, 40), (71, 12), (110, 70), (64, 33)] {
                let (a, b) = (painted.get_pixel(x, y), cache.get_pixel(x, y));
                assert!(
                    close(srgb_encode(a.r), srgb_encode(b.r), 4e-3),
                    "{style:?} at ({x}, {y}): painted {a:?}, fill {b:?}"
                );
            }
        }
        // The selection becomes the fill layer's mask.
        let (mut d, _) = doc(40, 40);
        d.selection = Some(Selection::rect(Rect::new(0, 0, 20, 40)));
        GradientFillLayer {
            paint: bw(GradientStyle::Linear, (0.0, 0.0), (40.0, 0.0)),
            above: None,
        }
        .apply(&mut d)
        .unwrap();
        let m = d.layers().last().unwrap().mask.as_ref().unwrap();
        assert_eq!((m.value(5, 5), m.value(30, 5)), (1.0, 0.0));
    }

    #[test]
    fn fill_settings_for_a_drag() {
        // A 40 px drag to the right, centred on a 100 × 50 canvas.
        let p = bw(GradientStyle::Linear, (30.0, 25.0), (70.0, 25.0));
        let Fill::Gradient {
            angle, scale, offset, ..
        } = fill_for_drag(&p, Rect::new(0, 0, 100, 50))
        else {
            panic!()
        };
        assert!(close(angle, 0.0, 1e-4) && close(scale, 0.4, 1e-6) && offset == [0.0, 0.0]);
        // Radial from the top-left quarter point, dragged straight up 10 px.
        let p = bw(GradientStyle::Radial, (25.0, 12.5), (25.0, 2.5));
        let Fill::Gradient {
            angle, scale, offset, ..
        } = fill_for_drag(&p, Rect::new(0, 0, 100, 50))
        else {
            panic!()
        };
        assert!(close(angle, 90.0, 1e-4), "{angle}");
        assert!(close(scale, 20.0 / 50.0, 1e-5), "{scale}");
        assert!(close(offset[0], -0.25, 1e-6) && close(offset[1], -0.25, 1e-6));
    }
}
