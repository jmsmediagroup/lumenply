//! Camera Raw "Basic" develop settings. `lumenply-render::develop` runs
//! them, both when a camera RAW file is opened and as the Camera Raw Filter
//! ([`crate::Filter::Develop`]) on any layer.
//!
//! The local controls (highlights, shadows, texture, clarity, dehaze) read
//! a blurred neighbourhood whose radius is a fixed fraction of the image's
//! longer side, so a develop looks the same at preview and full size. For
//! the filter that "image" is the canvas rectangle stored with it.

use serde::{Deserialize, Serialize};

fn fifty() -> f32 {
    50.0
}

/// The Basic panel. Every slider is -100..=100 except `exposure` (EV,
/// -5..=5) and `vignette_midpoint` (0..=100); [`Develop::NEUTRAL`] leaves
/// pixels untouched.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Develop {
    #[serde(default)]
    pub temperature: f32,
    #[serde(default)]
    pub tint: f32,
    #[serde(default)]
    pub exposure: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub highlights: f32,
    #[serde(default)]
    pub shadows: f32,
    #[serde(default)]
    pub whites: f32,
    #[serde(default)]
    pub blacks: f32,
    #[serde(default)]
    pub vibrance: f32,
    #[serde(default)]
    pub saturation: f32,
    /// A gentle film-like S curve, as raw converters apply by default so a
    /// linear develop doesn't look flat. Off for the Camera Raw Filter,
    /// whose input is already a finished image.
    #[serde(default)]
    pub tone_curve: bool,
    /// Medium-sized detail (an unsharp mask on log luminance, small radius).
    #[serde(default)]
    pub texture: f32,
    /// Local mid-tone contrast (an unsharp mask on log luminance, mid-size
    /// radius, weighted towards the mid-tones).
    #[serde(default)]
    pub clarity: f32,
    /// Removes (or adds) a veil estimated from the blurred darkest channel.
    #[serde(default)]
    pub dehaze: f32,
    /// Post-crop vignette: negative darkens the corners, positive lightens.
    #[serde(default)]
    pub vignette: f32,
    /// Where the vignette starts: 0 near the centre, 100 near the corners.
    #[serde(default = "fifty")]
    pub vignette_midpoint: f32,
}

impl Default for Develop {
    /// What opening a raw file starts with: neutral sliders, tone curve on.
    fn default() -> Self {
        Develop {
            tone_curve: true,
            ..Develop::NEUTRAL
        }
    }
}

/// Box radii (px) of the local controls on one image size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevelopRadii {
    /// Highlights, shadows and the dehaze veil: 2 % of the longer side.
    pub tone: usize,
    /// Clarity: 1 % of the longer side.
    pub clarity: usize,
    /// Texture: 0.25 % of the longer side.
    pub texture: usize,
}

/// Images larger than this measure their local controls as if they were
/// this size, so a corrupt frame can't size an absurd filter reach.
const MAX_SIZE: f32 = 20_000.0;

fn finite(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        0.0
    }
}

impl Develop {
    /// Every control at rest: the identity.
    pub const NEUTRAL: Develop = Develop {
        temperature: 0.0,
        tint: 0.0,
        exposure: 0.0,
        contrast: 0.0,
        highlights: 0.0,
        shadows: 0.0,
        whites: 0.0,
        blacks: 0.0,
        vibrance: 0.0,
        saturation: 0.0,
        tone_curve: false,
        texture: 0.0,
        clarity: 0.0,
        dehaze: 0.0,
        vignette: 0.0,
        vignette_midpoint: 50.0,
    };

    /// The settings with every value finite and inside its slider's range
    /// (NaN and infinities from a damaged file become 0).
    pub fn sane(&self) -> Develop {
        let s = |v: f32| finite(v, -100.0, 100.0);
        Develop {
            temperature: s(self.temperature),
            tint: s(self.tint),
            exposure: finite(self.exposure, -5.0, 5.0),
            contrast: s(self.contrast),
            highlights: s(self.highlights),
            shadows: s(self.shadows),
            whites: s(self.whites),
            blacks: s(self.blacks),
            vibrance: s(self.vibrance),
            saturation: s(self.saturation),
            tone_curve: self.tone_curve,
            texture: s(self.texture),
            clarity: s(self.clarity),
            dehaze: s(self.dehaze),
            vignette: s(self.vignette),
            vignette_midpoint: if self.vignette_midpoint.is_finite() {
                self.vignette_midpoint.clamp(0.0, 100.0)
            } else {
                50.0
            },
        }
    }

    /// True when the settings change nothing (the midpoint alone does not
    /// count: without a vignette it has nothing to place).
    pub fn is_neutral(&self) -> bool {
        Develop {
            vignette_midpoint: 50.0,
            ..self.sane()
        } == Develop::NEUTRAL
    }

    /// White balance and exposure as per-channel linear gains. The
    /// temperature/tint shift keeps a neutral grey's luminance.
    pub fn gains(&self) -> [f32; 3] {
        let t = self.temperature / 100.0;
        let g = self.tint / 100.0;
        let mut k = [(0.45 * t).exp2(), (-0.3 * g).exp2(), (-0.45 * t).exp2()];
        let y = 0.2126 * k[0] + 0.7152 * k[1] + 0.0722 * k[2];
        let e = self.exposure.exp2();
        for c in &mut k {
            *c = *c / y * e;
        }
        k
    }

    /// Highlights or shadows are set (they need the large-scale base).
    pub fn is_local(&self) -> bool {
        self.highlights != 0.0 || self.shadows != 0.0
    }

    /// The local controls' radii on an image whose longer side is `size`.
    pub fn radii(size: u32) -> DevelopRadii {
        let s = (size as f32).clamp(1.0, MAX_SIZE);
        let r = |f: f32| (s * f).round().max(1.0) as usize;
        DevelopRadii {
            tone: r(0.02),
            clarity: r(0.01),
            texture: r(0.0025),
        }
    }

    /// How far (px) the develop reads around a pixel on an image whose
    /// longer side is `size`: three box passes of the widest radius in use.
    pub fn reach(&self, size: u32) -> i32 {
        let d = self.sane();
        let r = Develop::radii(size);
        let mut m = 0;
        if d.is_local() || d.dehaze != 0.0 {
            m = m.max(r.tone);
        }
        if d.clarity != 0.0 {
            m = m.max(r.clarity);
        }
        if d.texture != 0.0 {
            m = m.max(r.texture);
        }
        3 * m as i32
    }
}

/// The longer side of a filter frame `[x, y, w, h]`, at least 1.
pub fn frame_size(frame: [i32; 4]) -> u32 {
    frame[2].max(frame[3]).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_neutral_and_the_raw_default_has_the_curve() {
        assert!(Develop::NEUTRAL.is_neutral());
        assert!(!Develop::default().is_neutral());
        let mid = Develop {
            vignette_midpoint: 80.0,
            ..Develop::NEUTRAL
        };
        assert!(mid.is_neutral(), "a midpoint without a vignette does nothing");
        let nan = Develop {
            clarity: f32::NAN,
            ..Develop::NEUTRAL
        };
        assert!(nan.is_neutral(), "NaN reads as 0");
    }

    #[test]
    fn radii_and_reach_scale_with_the_image() {
        assert_eq!(
            Develop::radii(4000),
            DevelopRadii {
                tone: 80,
                clarity: 40,
                texture: 10
            }
        );
        assert_eq!(
            Develop::radii(10),
            DevelopRadii {
                tone: 1,
                clarity: 1,
                texture: 1
            }
        );
        assert_eq!(Develop::NEUTRAL.reach(4000), 0);
        let exposure = Develop {
            exposure: 1.0,
            ..Develop::NEUTRAL
        };
        assert_eq!(exposure.reach(4000), 0, "global controls read no neighbours");
        let shadows = Develop {
            shadows: 40.0,
            ..Develop::NEUTRAL
        };
        assert_eq!(shadows.reach(4000), 240);
        let clarity = Develop {
            clarity: 40.0,
            ..Develop::NEUTRAL
        };
        assert_eq!(clarity.reach(4000), 120);
        let texture = Develop {
            texture: 40.0,
            ..Develop::NEUTRAL
        };
        assert_eq!(texture.reach(4000), 30);
        // A corrupt frame can't size an absurd reach.
        assert_eq!(shadows.reach(u32::MAX), 1200);
        assert_eq!(frame_size([0, 0, 300, 200]), 300);
        assert_eq!(frame_size([0, 0, -5, 0]), 1);
    }

    #[test]
    fn settings_from_older_files_fill_in_the_new_controls() {
        let old = r#"{"temperature":10.0,"tint":0.0,"exposure":0.5,"contrast":0.0,
            "highlights":0.0,"shadows":0.0,"whites":0.0,"blacks":0.0,
            "vibrance":0.0,"saturation":0.0,"tone_curve":true}"#;
        let d: Develop = serde_json::from_str(old).unwrap();
        assert_eq!(
            d,
            Develop {
                temperature: 10.0,
                exposure: 0.5,
                tone_curve: true,
                ..Develop::NEUTRAL
            }
        );
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Develop>(&s).unwrap(), d);
    }
}
