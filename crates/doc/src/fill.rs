//! Fill layers: a solid colour or a gradient that covers the whole canvas
//! and composites like a pixel layer (mask, blend mode, opacity, effects
//! and clipping all apply), but stays editable.
//!
//! The pixels to composite live in [`FillLayer::cache`], derived state
//! rendered by `lumenply-render` for the current canvas size — never saved,
//! rebuilt on load, on every edit and whenever the canvas changes size.

use lumenply_tiles::{Rect, Rgba, TileStore};
use serde::{Deserialize, Serialize};

use crate::adjust::srgb_decode;
use crate::gradient::Gradient;

/// The shape a gradient fill takes across the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GradientStyle {
    #[default]
    Linear,
    Radial,
    /// Sweeps once around the centre, starting along the angle.
    Angle,
    /// Linear, mirrored about the centre line.
    Reflected,
    Diamond,
}

impl GradientStyle {
    pub const ALL: [GradientStyle; 5] = [
        GradientStyle::Linear,
        GradientStyle::Radial,
        GradientStyle::Angle,
        GradientStyle::Reflected,
        GradientStyle::Diamond,
    ];

    pub fn name(self) -> &'static str {
        match self {
            GradientStyle::Linear => "Linear",
            GradientStyle::Radial => "Radial",
            GradientStyle::Angle => "Angle",
            GradientStyle::Reflected => "Reflected",
            GradientStyle::Diamond => "Diamond",
        }
    }
}

/// What a fill layer paints.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Fill {
    Solid {
        /// Straight linear RGB.
        color: [f32; 3],
    },
    Gradient {
        gradient: Gradient,
        style: GradientStyle,
        /// Direction in degrees: 0 runs left → right, 90 bottom → top.
        angle: f32,
        /// Length relative to the canvas span along the angle; 1.0 = 100%.
        scale: f32,
        reverse: bool,
        /// Centre offset as a fraction of the canvas width and height.
        #[serde(default)]
        offset: [f32; 2],
    },
}

impl Fill {
    pub fn name(&self) -> &'static str {
        match self {
            Fill::Solid { .. } => "Color Fill",
            Fill::Gradient { .. } => "Gradient Fill",
        }
    }

    pub fn gradient_default() -> Fill {
        Fill::Gradient {
            gradient: Gradient::default(),
            style: GradientStyle::Linear,
            angle: 90.0,
            scale: 1.0,
            reverse: false,
            offset: [0.0, 0.0],
        }
    }

    /// A per-pixel evaluator for a canvas of `canvas` size.
    pub fn sampler(&self, canvas: Rect) -> FillSampler {
        match self {
            Fill::Solid { color } => {
                let c = color.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
                FillSampler::Solid(Rgba::new(c[0], c[1], c[2], 1.0))
            }
            Fill::Gradient {
                gradient,
                style,
                angle,
                scale,
                reverse,
                offset,
            } => {
                let table: Vec<Rgba> = gradient
                    .sample_gamma(TABLE)
                    .into_iter()
                    .map(|[r, g, b, a]| {
                        Rgba::from_straight(srgb_decode(r), srgb_decode(g), srgb_decode(b), a)
                    })
                    .collect();
                let a = if angle.is_finite() {
                    angle.to_radians()
                } else {
                    0.0
                };
                let (w, h) = (canvas.w.max(1) as f32, canvas.h.max(1) as f32);
                let dir = (a.cos(), -a.sin()); // canvas y points down
                let scale = if scale.is_finite() {
                    scale.clamp(0.01, 100.0)
                } else {
                    1.0
                };
                // The span of the canvas projected onto the direction.
                let span = ((w * dir.0).abs() + (h * dir.1).abs()).max(1.0) * scale;
                let off = offset.map(|v| if v.is_finite() { v.clamp(-10.0, 10.0) } else { 0.0 });
                FillSampler::Gradient {
                    table,
                    style: *style,
                    centre: (
                        canvas.x as f32 + w / 2.0 + off[0] * w,
                        canvas.y as f32 + h / 2.0 + off[1] * h,
                    ),
                    dir,
                    span,
                    reverse: *reverse,
                }
            }
        }
    }
}

/// Entries in a gradient fill's colour table.
const TABLE: usize = 1024;

/// Evaluates a [`Fill`] per pixel (premultiplied linear RGBA).
#[derive(Clone, Debug)]
pub enum FillSampler {
    Solid(Rgba),
    Gradient {
        table: Vec<Rgba>,
        style: GradientStyle,
        centre: (f32, f32),
        /// Unit direction of the angle, in canvas coordinates.
        dir: (f32, f32),
        /// Full length of a linear ramp, in pixels.
        span: f32,
        reverse: bool,
    },
}

impl FillSampler {
    /// Where pixel `(x, y)` (its centre) falls along the gradient, 0..1.
    pub fn position(&self, x: i32, y: i32) -> f32 {
        let FillSampler::Gradient {
            style,
            centre,
            dir,
            span,
            reverse,
            ..
        } = self
        else {
            return 0.0;
        };
        let (px, py) = (x as f32 + 0.5 - centre.0, y as f32 + 0.5 - centre.1);
        let along = px * dir.0 + py * dir.1;
        let across = -px * dir.1 + py * dir.0;
        let half = span / 2.0;
        let t = match style {
            GradientStyle::Linear => along / span + 0.5,
            GradientStyle::Reflected => along.abs() / half,
            GradientStyle::Radial => (px * px + py * py).sqrt() / half,
            GradientStyle::Diamond => (along.abs() + across.abs()) / half,
            GradientStyle::Angle => {
                // Counter-clockwise from the direction, as on screen.
                let a = (-across).atan2(along);
                a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
            }
        };
        let t = t.clamp(0.0, 1.0);
        if *reverse {
            1.0 - t
        } else {
            t
        }
    }

    #[inline]
    pub fn sample(&self, x: i32, y: i32) -> Rgba {
        match self {
            FillSampler::Solid(c) => *c,
            FillSampler::Gradient { table, .. } => {
                let p = self.position(x, y) * (table.len() - 1) as f32;
                let i = p as usize;
                if i + 1 >= table.len() {
                    return table[table.len() - 1];
                }
                let k = p - i as f32;
                let (a, b) = (table[i], table[i + 1]);
                Rgba::new(
                    a.r + (b.r - a.r) * k,
                    a.g + (b.g - a.g) * k,
                    a.b + (b.b - a.b) * k,
                    a.a + (b.a - a.a) * k,
                )
            }
        }
    }
}

/// A fill layer's settings plus its rendered pixels.
#[derive(Clone, Debug)]
pub struct FillLayer {
    pub fill: Fill,
    /// Derived: `fill` rendered over the canvas recorded in `cache_canvas`.
    pub cache: Option<TileStore>,
    /// Canvas size the cache was rendered for.
    pub cache_canvas: (u32, u32),
}

impl FillLayer {
    pub fn new(fill: Fill) -> Self {
        FillLayer {
            fill,
            cache: None,
            cache_canvas: (0, 0),
        }
    }

    /// Whether the cache is missing or was rendered for another canvas.
    pub fn is_stale(&self, width: u32, height: u32) -> bool {
        self.cache.is_none() || self.cache_canvas != (width, height)
    }
}

impl PartialEq for FillLayer {
    /// Compares the settings; the cache is derived state.
    fn eq(&self, o: &Self) -> bool {
        self.fill == o.fill
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjust::srgb_encode;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    fn bw(style: GradientStyle, angle: f32) -> Fill {
        Fill::Gradient {
            gradient: Gradient::default(),
            style,
            angle,
            scale: 1.0,
            reverse: false,
            offset: [0.0, 0.0],
        }
    }

    #[test]
    fn solid_fills_are_one_opaque_colour() {
        let s = Fill::Solid {
            color: [0.5, 0.25, 1.0],
        }
        .sampler(Rect::new(0, 0, 10, 10));
        let p = s.sample(3, 7);
        assert_eq!([p.r, p.g, p.b, p.a], [0.5, 0.25, 1.0, 1.0]);
    }

    #[test]
    fn gradient_styles_place_pixels_along_the_ramp() {
        let canvas = Rect::new(0, 0, 100, 100);
        // Linear at 0°: left edge 0, centre 0.5 (sRGB mid grey), right edge 1.
        let lin = bw(GradientStyle::Linear, 0.0).sampler(canvas);
        assert!(close(lin.position(0, 50), 0.005), "{}", lin.position(0, 50));
        assert!(close(lin.position(49, 10), 0.495));
        assert!(close(lin.position(99, 50), 0.995));
        let mid = lin.sample(49, 50);
        assert!(close(srgb_encode(mid.r), 0.495), "interpolated in gamma: {mid:?}");
        // At 90° it runs bottom → top.
        let up = bw(GradientStyle::Linear, 90.0).sampler(canvas);
        assert!(up.position(50, 99) < 0.01 && up.position(50, 0) > 0.99);
        // Radial: 0 at the centre, 1 at half the span (the edge midpoints).
        let rad = bw(GradientStyle::Radial, 0.0).sampler(canvas);
        assert!(close(rad.position(49, 49), 0.0141));
        assert!(close(rad.position(99, 49), 0.99));
        // Reflected mirrors about the centre line.
        let refl = bw(GradientStyle::Reflected, 0.0).sampler(canvas);
        assert!(close(refl.position(24, 0), refl.position(75, 99)));
        assert!(close(refl.position(24, 0), 0.51));
        // Diamond: |along| + |across| over half the span.
        let dia = bw(GradientStyle::Diamond, 0.0).sampler(canvas);
        assert!(close(dia.position(74, 74), 0.98));
        assert!(close(dia.position(59, 49), 0.2));
        // Angle sweeps counter-clockwise from the direction: straight up
        // from the centre is a quarter turn.
        let ang = bw(GradientStyle::Angle, 0.0).sampler(canvas);
        assert!(close(ang.position(99, 49), 0.0), "{}", ang.position(99, 49));
        assert!(close(ang.position(49, 0), 0.25), "{}", ang.position(49, 0));
        assert!(close(ang.position(0, 49), 0.5), "{}", ang.position(0, 49));
    }

    #[test]
    fn reverse_scale_and_offset_reshape_the_ramp() {
        let canvas = Rect::new(0, 0, 100, 100);
        let mut f = bw(GradientStyle::Linear, 0.0);
        if let Fill::Gradient { reverse, .. } = &mut f {
            *reverse = true;
        }
        assert!(close(f.sampler(canvas).position(0, 0), 0.995));
        let mut half = bw(GradientStyle::Linear, 0.0);
        if let Fill::Gradient { scale, offset, .. } = &mut half {
            *scale = 0.5;
            *offset = [0.25, 0.0];
        }
        let s = half.sampler(canvas);
        // Centre moved to x = 75, ramp 50 px long: 50..100.
        assert!(close(s.position(49, 0), 0.0) && close(s.position(74, 0), 0.49));
        assert!(close(s.position(99, 0), 0.99));
    }

    #[test]
    fn fills_serialize_with_their_kind_tag() {
        let f = Fill::Solid {
            color: [1.0, 0.0, 0.0],
        };
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, r#"{"type":"solid","color":[1.0,0.0,0.0]}"#);
        let g = bw(GradientStyle::Diamond, 30.0);
        let back: Fill = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        // The offset may be absent.
        let old = r#"{"type":"gradient","gradient":{"stops":[]},"style":"radial","angle":0,"scale":1,"reverse":false}"#;
        assert!(matches!(
            serde_json::from_str::<Fill>(old).unwrap(),
            Fill::Gradient {
                offset: [0.0, 0.0],
                style: GradientStyle::Radial,
                ..
            }
        ));
    }
}
