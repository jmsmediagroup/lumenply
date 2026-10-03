//! Brush tips and shape dynamics: Photoshop's Brush Settings for the
//! engine. The code lives in [`lumenply_render::brush_tip`], where the
//! edit graph can replay strokes too (ADR 0025); this module re-exports it
//! under its old path and keeps the tests that paint through
//! [`crate::commands::PaintStroke`].

pub use lumenply_render::brush_tip::{
    builtin_tip, grain, grain_mask, BrushDynamics, BrushTip, Dab, TipSampler, BUILTIN_TIPS, MAX_SCATTER,
    MAX_TIP_SIDE, TIP_MARGIN,
};

#[cfg(test)]
use lumenply_render::brush_tip::{hash01, stamp, SALT_FG_BG};
#[cfg(test)]
use lumenply_tiles::Rect;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
mod stroke_tests {
    use super::*;
    use crate::commands::{Brush, BrushMode, PaintStroke, StrokePoint};
    use crate::Command;
    use lumenply_doc::{Document, LayerId};
    use lumenply_tiles::Rgba;

    /// The round-brush painter as it was before tips and dynamics, kept
    /// verbatim so the default brush is proven to render bit-identically.
    mod legacy {
        use super::super::hash01;
        use crate::commands::{smooth_stroke, Brush, StrokePoint};
        use lumenply_doc::Selection;
        use lumenply_tiles::Rect;

        pub fn interpolate_dabs(brush: &Brush, points: &[StrokePoint]) -> Vec<StrokePoint> {
            let smoothed = smooth_stroke(points);
            let points = &smoothed[..];
            let mut dabs = vec![points[0]];
            let mut carry = 0.0f32;
            let jitter = brush.jitter.clamp(0.0, 1.0);
            for pair in points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                if len <= 0.0 {
                    continue;
                }
                let eff = brush.radius * (0.5 * (a.pressure + b.pressure)).clamp(0.0, 1.0);
                let step = (brush.spacing * eff).max(0.5);
                let mut t = step - carry;
                while t <= len {
                    let f = t / len;
                    dabs.push(
                        StrokePoint::new(
                            a.x + (b.x - a.x) * f,
                            a.y + (b.y - a.y) * f,
                            a.pressure + (b.pressure - a.pressure) * f,
                        )
                        .with_opacity(a.opacity + (b.opacity - a.opacity) * f),
                    );
                    t += step;
                }
                carry = len - (t - step);
            }
            if jitter > 0.0 {
                for (i, d) in dabs.iter_mut().enumerate() {
                    let angle = hash01(i as u32, 0x9E37_79B9) * std::f32::consts::TAU;
                    let dist = hash01(i as u32, 0x85EB_CA6B).sqrt() * jitter * brush.radius;
                    d.x += angle.cos() * dist;
                    d.y += angle.sin() * dist;
                }
            }
            dabs
        }

        pub fn dab_coverage(
            brush: &Brush,
            p: StrokePoint,
            canvas: Rect,
            sel: Option<&Selection>,
            mut f: impl FnMut(i32, i32, f32),
        ) {
            let r = brush.radius * p.pressure.clamp(0.0, 1.0);
            if r <= 0.0 {
                return;
            }
            let x0 = (p.x - r).floor() as i32;
            let y0 = (p.y - r).floor() as i32;
            let x1 = (p.x + r).ceil() as i32;
            let y1 = (p.y + r).ceil() as i32;
            let area = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32).intersect(&canvas);
            let hard = brush.hardness.clamp(0.0, 0.999);
            for py in area.y..area.bottom() {
                for px in area.x..area.right() {
                    let dx = px as f32 + 0.5 - p.x;
                    let dy = py as f32 + 0.5 - p.y;
                    let d = (dx * dx + dy * dy).sqrt() / r;
                    let edge = ((1.0 - d) * r + 0.5).clamp(0.0, 1.0);
                    let soft = if d <= hard {
                        1.0
                    } else {
                        1.0 - (d - hard) / (1.0 - hard)
                    };
                    let cover = edge
                        * soft.clamp(0.0, 1.0)
                        * sel.map_or(1.0, |s| s.value(px, py))
                        * p.opacity.clamp(0.0, 1.0);
                    if cover > 0.0 {
                        f(px, py, cover);
                    }
                }
            }
        }
    }

    fn doc() -> (Document, LayerId) {
        let mut doc = Document::new(96, 64);
        let id = doc.add_pixel_layer("p");
        (doc, id)
    }

    fn paint(brush: Brush, points: Vec<StrokePoint>) -> Document {
        let (mut d, id) = doc();
        PaintStroke {
            layer: id,
            brush,
            points,
        }
        .apply(&mut d)
        .unwrap();
        d
    }

    fn px(d: &Document, x: i32, y: i32) -> Rgba {
        d.layers()[0].pixels().unwrap().get_pixel(x, y)
    }

    #[test]
    fn the_round_tip_renders_exactly_as_before() {
        let wavy: Vec<StrokePoint> = (0..30)
            .map(|i| {
                let t = i as f32 / 29.0;
                StrokePoint::new(8.0 + 80.0 * t, 32.0 + 14.0 * (t * 6.0).sin(), 0.3 + 0.7 * t)
                    .with_opacity(1.0 - 0.6 * t)
            })
            .collect();
        let brushes = [
            Brush::default(),
            Brush {
                radius: 11.5,
                hardness: 0.0,
                color: [0.9, 0.2, 0.1, 0.7],
                spacing: 0.1,
                ..Brush::default()
            },
            Brush {
                radius: 6.0,
                hardness: 1.0,
                jitter: 0.8,
                // A circle has no angle: turning it changes nothing.
                angle: 37.0,
                ..Brush::default()
            },
        ];
        for (n, brush) in brushes.into_iter().enumerate() {
            let new = paint(brush.clone(), wavy.clone());
            let (mut old, id) = doc();
            let canvas = old.canvas();
            let store = old.layer_mut(id).unwrap().pixels_mut().unwrap();
            let [cr, cg, cb, ca] = brush.color;
            for d in legacy::interpolate_dabs(&brush, &wavy) {
                legacy::dab_coverage(&brush, d, canvas, None, |x, y, cover| {
                    let dst = store.get_pixel(x, y);
                    store.set_pixel(x, y, Rgba::from_straight(cr, cg, cb, ca * cover).over(dst));
                });
            }
            let mut painted = 0;
            for y in 0..64 {
                for x in 0..96 {
                    let (a, b) = (px(&new, x, y), px(&old, x, y));
                    assert_eq!(
                        (a.r.to_bits(), a.g.to_bits(), a.b.to_bits(), a.a.to_bits()),
                        (b.r.to_bits(), b.g.to_bits(), b.b.to_bits(), b.a.to_bits()),
                        "brush {n} differs at ({x}, {y})"
                    );
                    painted += (a.a > 0.0) as u32;
                }
            }
            assert!(painted > 300, "brush {n} painted {painted} px");
        }
        // And one explicit value: the default brush's centre is solid black.
        let d = paint(Brush::default(), vec![StrokePoint::new(20.5, 20.5, 1.0)]);
        assert_eq!(px(&d, 20, 20).a, 1.0);
        assert_eq!(px(&d, 20 + 9, 20).a, 0.0);
    }

    /// 4×4, left half full paint, right half empty.
    fn half_tip() -> Arc<BrushTip> {
        let cov = (0..16).map(|i| if i % 4 < 2 { 1.0 } else { 0.0 }).collect();
        Arc::new(BrushTip::new("half", 4, 4, cov).unwrap())
    }

    #[test]
    fn a_sampled_tip_stamps_scaled_with_bilinear_edges() {
        // Radius 4 = an 8 px dab: each tip pixel covers 2×2 canvas pixels.
        let brush = Brush {
            radius: 4.0,
            tip: Some(half_tip()),
            ..Brush::default()
        };
        let d = paint(brush, vec![StrokePoint::new(10.0, 10.0, 1.0)]);
        // The edge fades across one texel (two pixels), centred on it.
        let row: Vec<f32> = (4..12).map(|x| px(&d, x, 10).a).collect();
        assert_eq!(row, vec![0.0, 0.25, 0.75, 1.0, 1.0, 0.75, 0.25, 0.0]);
        // The top and bottom edges fade in the same way.
        let col: Vec<f32> = [4, 5, 6, 13, 14, 15].iter().map(|&y| px(&d, 7, y).a).collect();
        assert_eq!(col, vec![0.0, 0.25, 0.75, 0.75, 0.25, 0.0]);
    }

    #[test]
    fn tips_turn_counter_clockwise_and_squash() {
        // An 8×2 bar → 16×4 px at radius 8; at 90° it stands upright.
        let bar = Arc::new(BrushTip::new("bar", 8, 2, vec![1.0; 16]).unwrap());
        let flat = Brush {
            radius: 8.0,
            tip: Some(bar.clone()),
            ..Brush::default()
        };
        let d = paint(flat.clone(), vec![StrokePoint::new(30.0, 30.0, 1.0)]);
        assert_eq!((px(&d, 36, 30).a, px(&d, 30, 36).a), (1.0, 0.0));
        let upright = Brush { angle: 90.0, ..flat };
        let d = paint(upright, vec![StrokePoint::new(30.0, 30.0, 1.0)]);
        assert_eq!((px(&d, 36, 30).a, px(&d, 30, 36).a), (0.0, 1.0));
        assert_eq!(px(&d, 30, 23).a, 1.0);
        // The round tip at 50 % roundness is a 20×10 ellipse.
        let oval = Brush {
            radius: 10.0,
            hardness: 1.0,
            roundness: 0.5,
            ..Brush::default()
        };
        let d = paint(oval.clone(), vec![StrokePoint::new(40.5, 30.5, 1.0)]);
        assert_eq!((px(&d, 47, 30).a, px(&d, 40, 33).a), (1.0, 1.0));
        assert_eq!((px(&d, 40, 37).a, px(&d, 52, 30).a), (0.0, 0.0));
        // Turned 90°: now tall.
        let d = paint(
            Brush { angle: 90.0, ..oval },
            vec![StrokePoint::new(40.5, 30.5, 1.0)],
        );
        assert_eq!((px(&d, 40, 37).a, px(&d, 47, 30).a), (1.0, 0.0));
    }

    #[test]
    fn minified_tips_are_stamped_whole_without_a_clipped_fade() {
        // Solid tips touching their borders, stamped small (coarse mip
        // levels with zero padding), squashed and turned: the stamp must
        // visit every pixel the sampler gives coverage to, and all of them
        // lie within radius × reach + TIP_MARGIN (what stroke_bounds uses).
        for (side, radius, rho, angle) in [
            (200u32, 25.0f32, 0.25f32, 30f32),
            (100, 6.0, 1.0, 0.0),
            (37, 3.3, 0.6, 75.0),
            (8, 20.0, 0.4, -20.0),
        ] {
            let tip = BrushTip::new("solid", side, side, vec![1.0; (side * side) as usize]).unwrap();
            let d = Dab {
                x: 60.3,
                y: 50.7,
                radius,
                opacity: 1.0,
                angle: angle.to_radians(),
                roundness: rho,
                flip_x: false,
                flip_y: false,
                color: None,
            };
            let mut got = std::collections::HashMap::new();
            stamp(Some(&tip), 1.0, &d, Rect::new(0, 0, 120, 100), |x, y, c| {
                got.insert((x, y), c);
            });
            let scale = 2.0 * radius / side as f32;
            let s = tip.sampler((1.0 / (scale * rho)).min(2.0 / scale));
            let (sin, cos) = d.angle.sin_cos();
            let mut painted = 0;
            for y in 0..100 {
                for x in 0..120 {
                    let (dx, dy) = (x as f32 + 0.5 - d.x, y as f32 + 0.5 - d.y);
                    let lx = dx * cos - dy * sin;
                    let ly = (dx * sin + dy * cos) / rho;
                    let half = side as f32 * 0.5;
                    let want = s.at(lx / scale + half, ly / scale + half);
                    let have = got.get(&(x, y)).copied().unwrap_or(0.0);
                    assert_eq!(have, want, "side {side} at ({x}, {y})");
                    if want > 0.0 {
                        painted += 1;
                        assert!(dx.hypot(dy) <= radius * tip.reach() + TIP_MARGIN, "side {side}");
                    }
                }
            }
            assert!(painted > 10, "side {side}: {painted}");
        }
    }

    #[test]
    fn small_dabs_of_a_detailed_tip_average_instead_of_aliasing() {
        // A 64×64 one-pixel checkerboard stamped 4 px wide: without mips
        // each canvas pixel would land on a single 0 or 1 texel.
        let cov = (0..64 * 64).map(|i| ((i % 64 + i / 64) % 2) as f32).collect();
        let checker = Arc::new(BrushTip::new("checker", 64, 64, cov).unwrap());
        let brush = Brush {
            radius: 2.0,
            tip: Some(checker),
            ..Brush::default()
        };
        let d = paint(brush, vec![StrokePoint::new(10.0, 10.0, 1.0)]);
        for (x, y) in [(9, 9), (10, 9), (9, 10), (10, 10)] {
            assert!(
                (px(&d, x, y).a - 0.5).abs() < 0.02,
                "({x}, {y}) = {}",
                px(&d, x, y).a
            );
        }
    }

    #[test]
    fn colour_dynamics_give_each_dab_its_own_colour() {
        use lumenply_doc::adjust::{rgb_to_hsl, srgb_decode, srgb_encode};
        // Black toward a white background: the one dab's colour is the
        // gamma-space mix at its hashed amount.
        let brush = Brush {
            radius: 4.0,
            hardness: 1.0,
            dynamics: BrushDynamics {
                fg_bg_jitter: 1.0,
                ..BrushDynamics::default()
            },
            ..Brush::default()
        };
        let d = paint(brush, vec![StrokePoint::new(10.5, 10.5, 1.0)]);
        let want = srgb_decode(hash01(0, SALT_FG_BG));
        let p = px(&d, 10, 10);
        assert_eq!(p.a, 1.0);
        assert!(
            (p.r - want).abs() < 1e-5 && p.r == p.g && p.g == p.b,
            "{p:?} vs {want}"
        );
        // No colour dynamic: dabs carry no colour of their own.
        let line = [StrokePoint::new(0.0, 0.0, 1.0), StrokePoint::new(40.0, 0.0, 1.0)];
        let plain = crate::commands::interpolate_dabs(&Brush::default(), &line);
        assert!(plain.len() > 5 && plain.iter().all(|d| d.color.is_none()));
        // Hue jitter on red: hues spread within ±90° (50 % = a quarter
        // turn each way), lightness and saturation untouched.
        let red = Brush {
            color: [1.0, 0.0, 0.0, 1.0],
            dynamics: BrushDynamics {
                hue_jitter: 0.5,
                ..BrushDynamics::default()
            },
            ..Brush::default()
        };
        let dabs = crate::commands::interpolate_dabs(&red, &line);
        let mut hues = Vec::new();
        for d in &dabs {
            let c = d.color.expect("coloured").map(srgb_encode);
            let (h, s, l) = rgb_to_hsl(c[0], c[1], c[2]);
            assert!((s - 1.0).abs() < 1e-3 && (l - 0.5).abs() < 1e-3, "{s} {l}");
            let turn = if h > 0.5 { h - 1.0 } else { h };
            assert!(turn.abs() <= 0.25 + 1e-4, "{turn}");
            hues.push(turn);
        }
        assert!(hues.iter().any(|&h| h > 0.1) && hues.iter().any(|&h| h < -0.1));
        assert_eq!(
            dabs,
            crate::commands::interpolate_dabs(&red, &line),
            "replays match"
        );
    }

    #[test]
    fn grain_texture_keeps_a_canvas_anchored_tooth_bare() {
        let vals: Vec<f32> = (0..200).map(|i| grain(i * 3, i * 7, 1.0)).collect();
        assert!(vals.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(vals.contains(&0.0) && vals.iter().any(|&v| v > 0.8));
        assert_eq!(grain(5, 9, 1.0).to_bits(), grain(5, 9, 1.0).to_bits());
        // The mask is a soft threshold at the depth.
        assert_eq!(grain_mask(0.3, 0.5), 0.0);
        assert!((grain_mask(0.5, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(grain_mask(0.7, 0.5), 1.0);
        // Two dabs on one spot (count 2) at half depth: a solid tip pixel
        // keeps exactly 1 − (1 − m)² for its grain mask m, and where the
        // grain is below the depth the paper stays bare however the dabs
        // build up.
        let brush = Brush {
            radius: 30.0,
            hardness: 1.0,
            dynamics: BrushDynamics {
                texture_depth: 0.5,
                count: 2,
                ..BrushDynamics::default()
            },
            ..Brush::default()
        };
        let d = paint(brush, vec![StrokePoint::new(40.5, 32.5, 1.0)]);
        let (mut bare, mut full) = (0, 0);
        for y in 20..45 {
            for x in 28..53 {
                let m = grain_mask(grain(x, y, 1.0), 0.5);
                let want = 1.0 - (1.0 - m) * (1.0 - m);
                assert!((px(&d, x, y).a - want).abs() < 1e-5, "({x}, {y})");
                bare += (px(&d, x, y).a == 0.0) as u32;
                full += (want == 1.0) as u32;
            }
        }
        assert!(bare > 50 && full > 50, "{bare} bare and {full} solid pixels");
        // Depth 0 is off: the same dab is solid.
        let solid = paint(
            Brush {
                radius: 30.0,
                hardness: 1.0,
                ..Brush::default()
            },
            vec![StrokePoint::new(40.5, 32.5, 1.0)],
        );
        assert_eq!(px(&solid, 40, 32).a, 1.0);
    }

    #[test]
    fn dynamic_strokes_replay_identically_and_stay_in_bounds() {
        let brush = Brush {
            radius: 6.0,
            jitter: 2.0,
            tip: builtin_tip("Spatter"),
            dynamics: BrushDynamics {
                size_jitter: 0.8,
                angle_jitter: 1.0,
                roundness_jitter: 0.5,
                count: 3,
                count_jitter: 0.5,
                opacity_jitter: 0.6,
                flip_x_jitter: true,
                follow_direction: true,
                ..BrushDynamics::default()
            },
            ..Brush::default()
        };
        let points: Vec<StrokePoint> = (0..12)
            .map(|i| StrokePoint::new(20.0 + i as f32 * 5.0, 32.0, 1.0))
            .collect();
        let a = paint(brush.clone(), points.clone());
        let b = paint(brush.clone(), points.clone());
        let bounds = crate::commands::stroke_bounds(&brush, &points, a.canvas());
        let mut painted = 0;
        for y in 0..64 {
            for x in 0..96 {
                let (p, q) = (px(&a, x, y), px(&b, x, y));
                assert_eq!(p.a.to_bits(), q.a.to_bits(), "({x}, {y}) replays identically");
                if p.a > 0.0 {
                    painted += 1;
                    assert!(bounds.contains(x, y), "({x}, {y}) outside {bounds:?}");
                }
            }
        }
        assert!(painted > 200, "{painted}");
        // The mode still applies to sampled tips: an erase lifts paint.
        let erase = Brush {
            mode: BrushMode::Erase,
            tip: Some(half_tip()),
            radius: 4.0,
            dynamics: BrushDynamics::default(),
            jitter: 0.0,
            ..brush
        };
        let (mut d, id) = doc();
        crate::commands::Fill {
            layer: id,
            color: [1.0, 1.0, 1.0, 1.0],
        }
        .apply(&mut d)
        .unwrap();
        PaintStroke {
            layer: id,
            brush: erase,
            points: vec![StrokePoint::new(10.0, 10.0, 1.0)],
        }
        .apply(&mut d)
        .unwrap();
        assert_eq!(
            (px(&d, 7, 10).a, px(&d, 10, 10).a, px(&d, 11, 10).a),
            (0.0, 0.75, 1.0)
        );
    }
}
