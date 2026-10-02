//! Colour gradients shared by the Gradient Map adjustment and gradient fill
//! layers: two or more stops, each a position and a colour (plus an
//! opacity that only fills use).
//!
//! Colours rest as straight linear RGB like every other colour in the
//! document, but **interpolation runs on gamma-encoded (sRGB) values**, as
//! Photoshop's "classic" gradients do: a black → white ramp passes mid grey
//! (sRGB 0.5) at its centre, not linear 0.5. See ADR 0005.

use serde::{Deserialize, Serialize};

use crate::adjust::{srgb_decode, srgb_encode};

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// One colour stop of a [`Gradient`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// Position along the gradient, 0.0 to 1.0.
    pub pos: f32,
    /// Straight linear RGB.
    pub color: [f32; 3],
    /// Opacity at this stop (gradient fills; a gradient map ignores it).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub alpha: f32,
}

impl GradientStop {
    pub fn new(pos: f32, color: [f32; 3]) -> Self {
        GradientStop {
            pos,
            color,
            alpha: 1.0,
        }
    }

    /// A stop from 8-bit sRGB components (how presets are written).
    pub fn srgb8(pos: f32, rgb: [u8; 3]) -> Self {
        GradientStop::new(pos, rgb.map(|c| srgb_decode(c as f32 / 255.0)))
    }
}

/// A colour ramp of two or more stops. Stops need not be sorted; every
/// evaluation sorts a copy by position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub stops: Vec<GradientStop>,
}

impl Default for Gradient {
    /// Black → white.
    fn default() -> Self {
        Gradient::two([0.0; 3], [1.0; 3])
    }
}

impl Gradient {
    /// From `a` (start) to `b` (end), straight linear RGB.
    pub fn two(a: [f32; 3], b: [f32; 3]) -> Self {
        Gradient {
            stops: vec![GradientStop::new(0.0, a), GradientStop::new(1.0, b)],
        }
    }

    /// Stops sorted by position, sanitised (finite, positions in 0..=1).
    /// An empty gradient reads as black → white.
    pub fn sorted(&self) -> Vec<GradientStop> {
        let mut s: Vec<GradientStop> = self
            .stops
            .iter()
            .map(|st| GradientStop {
                pos: if st.pos.is_finite() {
                    st.pos.clamp(0.0, 1.0)
                } else {
                    0.0
                },
                color: st
                    .color
                    .map(|c| if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 }),
                alpha: if st.alpha.is_finite() {
                    st.alpha.clamp(0.0, 1.0)
                } else {
                    1.0
                },
            })
            .collect();
        s.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        if s.is_empty() {
            return Gradient::default().stops;
        }
        s
    }

    /// The same ramp running the other way.
    pub fn reversed(&self) -> Gradient {
        let mut stops: Vec<GradientStop> = self
            .stops
            .iter()
            .map(|s| GradientStop {
                pos: 1.0 - s.pos,
                ..*s
            })
            .collect();
        stops.reverse();
        Gradient { stops }
    }

    /// Gamma-encoded (sRGB) colour and opacity at `t` (0..1).
    pub fn eval_gamma(&self, t: f32) -> [f32; 4] {
        eval_sorted(&encode(&self.sorted()), t)
    }

    /// `n` evenly spaced samples of [`Gradient::eval_gamma`] from 0 to 1;
    /// one sort for the whole table.
    pub fn sample_gamma(&self, n: usize) -> Vec<[f32; 4]> {
        let enc = encode(&self.sorted());
        let last = (n.max(2) - 1) as f32;
        (0..n).map(|i| eval_sorted(&enc, i as f32 / last)).collect()
    }

    /// Ready-made ramps for the gradient map (8-bit sRGB stops).
    pub fn presets() -> Vec<(&'static str, Gradient)> {
        let g = |stops: &[(f32, [u8; 3])]| Gradient {
            stops: stops.iter().map(|(p, c)| GradientStop::srgb8(*p, *c)).collect(),
        };
        vec![
            ("Black, White", g(&[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])])),
            (
                "Sepia",
                g(&[(0.0, [20, 12, 6]), (0.5, [140, 98, 57]), (1.0, [250, 238, 212])]),
            ),
            ("Cyanotype", g(&[(0.0, [8, 28, 66]), (1.0, [222, 240, 248])])),
            ("Teal, Orange", g(&[(0.0, [12, 52, 66]), (1.0, [250, 168, 82])])),
            ("Violet, Gold", g(&[(0.0, [40, 10, 64]), (1.0, [250, 204, 84])])),
            (
                "Copper",
                g(&[(0.0, [0, 0, 0]), (0.55, [184, 115, 51]), (1.0, [255, 230, 200])]),
            ),
        ]
    }
}

/// Stops with their colours gamma-encoded (the interpolation domain).
fn encode(stops: &[GradientStop]) -> Vec<(f32, [f32; 4])> {
    stops
        .iter()
        .map(|s| {
            let [r, g, b] = s.color.map(srgb_encode);
            (s.pos, [r, g, b, s.alpha])
        })
        .collect()
}

fn eval_sorted(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let first = stops[0];
    let last = stops[stops.len() - 1];
    if t <= first.0 {
        return first.1;
    }
    if t >= last.0 {
        return last.1;
    }
    for w in stops.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let span = b.0 - a.0;
            if span <= 1e-6 {
                return b.1;
            }
            let k = (t - a.0) / span;
            return std::array::from_fn(|i| a.1[i] + (b.1[i] - a.1[i]) * k);
        }
    }
    last.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn interpolates_in_the_gamma_domain_between_sorted_stops() {
        // Black → white passes sRGB 0.5 at its centre.
        let bw = Gradient::default();
        let mid = bw.eval_gamma(0.5);
        assert!(
            close(mid[0], 0.5) && close(mid[1], 0.5) && close(mid[3], 1.0),
            "{mid:?}"
        );

        // Unsorted stops sort; ends clamp to the end colours.
        let g = Gradient {
            stops: vec![
                GradientStop::srgb8(1.0, [0, 0, 255]),
                GradientStop::srgb8(0.0, [255, 0, 0]),
                GradientStop::srgb8(0.5, [0, 255, 0]),
            ],
        };
        let q = g.eval_gamma(0.25);
        assert!(close(q[0], 0.5) && close(q[1], 0.5) && close(q[2], 0.0), "{q:?}");
        assert!(close(g.eval_gamma(-3.0)[0], 1.0) && close(g.eval_gamma(7.0)[2], 1.0));

        // Reversing mirrors positions.
        let r = g.reversed();
        assert!(close(r.eval_gamma(0.0)[2], 1.0) && close(r.eval_gamma(0.75)[0], 0.5));

        // Opacity interpolates like the colours.
        let fade = Gradient {
            stops: vec![
                GradientStop::new(0.0, [1.0; 3]),
                GradientStop {
                    alpha: 0.0,
                    ..GradientStop::new(1.0, [1.0; 3])
                },
            ],
        };
        assert!(close(fade.eval_gamma(0.25)[3], 0.75));

        // A table samples both ends exactly.
        let t = bw.sample_gamma(5);
        assert_eq!(t.len(), 5);
        assert!(close(t[0][0], 0.0) && close(t[2][0], 0.5) && close(t[4][0], 1.0));
    }

    #[test]
    fn degenerate_gradients_stay_defined() {
        let empty = Gradient { stops: vec![] };
        assert!(
            close(empty.eval_gamma(1.0)[0], 1.0),
            "empty reads as black → white"
        );
        let single = Gradient {
            stops: vec![GradientStop::srgb8(0.3, [255, 0, 0])],
        };
        assert!(close(single.eval_gamma(0.9)[0], 1.0) && close(single.eval_gamma(0.0)[0], 1.0));
        let nan = Gradient {
            stops: vec![GradientStop::new(f32::NAN, [f32::NAN, 0.5, 2.0])],
        };
        let v = nan.eval_gamma(0.5);
        assert!(v.iter().all(|c| c.is_finite()), "{v:?}");
    }

    #[test]
    fn serializes_without_default_alpha_and_loads_it_back() {
        let g = Gradient::two([0.0; 3], [1.0, 0.0, 0.0]);
        let json = serde_json::to_string(&g).unwrap();
        assert!(!json.contains("alpha"), "{json}");
        let back: Gradient = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.stops[1].alpha, 1.0);
    }
}
