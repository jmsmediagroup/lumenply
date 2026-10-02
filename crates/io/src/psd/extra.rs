//! PSD blocks for the adjustments added after the original codec:
//! Gradient Map (`grdm`), Channel Mixer (`mixr`), Photo Filter (`phfl`)
//! and Selective Color (`selc`). All four have documented binary layouts;
//! the writers follow psd-tools' reader (and ag-psd's for `mixr`'s four
//! records), and every writer is cross-checked with psd-tools in tests.
//!
//! Colours travel as 16-bit sRGB (Photoshop's colour space 0, RGB);
//! gradient locations are 0..4096, percentages are whole numbers.

use lumenply_doc::{Adjustment, Gradient, GradientStop};

use super::{put_u16, put_u32, Rd};
use crate::{linear_to_srgb_f, srgb_to_linear_f};

/// A gradient location in Photoshop units (0..4096).
const LOC: f32 = 4096.0;

fn put_i16(d: &mut Vec<u8>, v: i16) {
    d.extend_from_slice(&v.to_be_bytes());
}

/// Percent of a fraction, rounded and clamped to `±limit`.
fn pct(v: f32, limit: i32) -> i16 {
    let v = if v.is_finite() { v } else { 0.0 };
    ((v * 100.0).round() as i32).clamp(-limit, limit) as i16
}

/// A straight linear channel as a 16-bit sRGB sample.
fn srgb16(v: f32) -> u16 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    (linear_to_srgb_f(v) * 65535.0).round() as u16
}

fn linear_of16(v: u16) -> f32 {
    srgb_to_linear_f(v as f32 / 65535.0)
}

/// The tagged-block key and payload for one of the newer adjustments.
pub(super) fn adjustment_block(adj: &Adjustment) -> Option<(&'static [u8; 4], Vec<u8>)> {
    let mut d = Vec::new();
    let key: &'static [u8; 4] = match adj {
        Adjustment::GradientMap { gradient, reverse } => {
            put_u16(&mut d, 1); // version
            d.push(u8::from(*reverse));
            d.push(0); // dithered
            let name: Vec<u16> = "Custom\0".encode_utf16().collect();
            put_u32(&mut d, name.len() as u32);
            for u in name {
                put_u16(&mut d, u);
            }
            let stops = gradient.sorted();
            put_u16(&mut d, stops.len().min(u16::MAX as usize) as u16);
            for s in &stops {
                put_u32(&mut d, (s.pos * LOC).round() as u32);
                put_u32(&mut d, 50); // midpoint, percent
                put_u16(&mut d, 0); // colour space: RGB
                for c in s.color {
                    put_u16(&mut d, srgb16(c));
                }
                put_u16(&mut d, 0); // fourth component
                put_u16(&mut d, 0); // padding
            }
            put_u16(&mut d, 2); // transparency stops: opaque at both ends
            for loc in [0u32, LOC as u32] {
                put_u32(&mut d, loc);
                put_u32(&mut d, 50);
                put_u16(&mut d, 100);
            }
            put_u16(&mut d, 2); // expansion
            put_u16(&mut d, 0); // interpolation (smoothness): linear, like ours
            put_u16(&mut d, 32); // length
            put_u16(&mut d, 0); // mode
            put_u32(&mut d, 0); // random seed
            put_u16(&mut d, 0); // showing transparency
            put_u16(&mut d, 0); // using vector colour
            put_u32(&mut d, 0); // roughness
            put_u16(&mut d, 0); // colour model
            for _ in 0..4 {
                put_u16(&mut d, 0); // minimum colour
            }
            for _ in 0..4 {
                put_u16(&mut d, u16::MAX); // maximum colour
            }
            put_u16(&mut d, 0); // dummy
            while d.len() % 4 != 0 {
                d.push(0);
            }
            b"grdm"
        }
        Adjustment::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
            gray,
        } => {
            // Four records (red, green, blue, gray outputs), each R, G, B,
            // an unused CMYK slot and the constant, in percent.
            put_u16(&mut d, 1);
            put_u16(&mut d, u16::from(*monochrome));
            let rec = |d: &mut Vec<u8>, w: &[f32; 4]| {
                for v in &w[..3] {
                    put_i16(d, pct(*v, 200));
                }
                put_i16(d, 0);
                put_i16(d, pct(w[3], 200));
            };
            if *monochrome {
                // Photoshop puts the gray row first when monochrome.
                rec(&mut d, gray);
                rec(&mut d, red);
                rec(&mut d, green);
                rec(&mut d, blue);
            } else {
                rec(&mut d, red);
                rec(&mut d, green);
                rec(&mut d, blue);
                rec(&mut d, gray);
            }
            b"mixr"
        }
        Adjustment::PhotoFilter {
            color,
            density,
            preserve_luminosity,
        } => {
            put_u16(&mut d, 2); // version 2: an RGB colour record
            put_u16(&mut d, 0); // colour space: RGB
            for c in color {
                put_u16(&mut d, srgb16(*c));
            }
            put_u16(&mut d, 0);
            put_u32(&mut d, pct(*density, 100).max(0) as u32);
            d.push(u8::from(*preserve_luminosity));
            while d.len() % 4 != 0 {
                d.push(0);
            }
            b"phfl"
        }
        Adjustment::SelectiveColor { colors, absolute } => {
            put_u16(&mut d, 1);
            put_u16(&mut d, u16::from(*absolute));
            for _ in 0..4 {
                put_i16(&mut d, 0); // record 0 is reserved
            }
            for fam in colors {
                for v in fam {
                    put_i16(&mut d, pct(*v, 100));
                }
            }
            b"selc"
        }
        _ => return None,
    };
    Some((key, d))
}

/// Decode a newer adjustment block; `None` when the key is not one of
/// them or the data does not parse.
pub(super) fn parse_adjustment(key: &[u8], data: &[u8]) -> Option<Adjustment> {
    let mut d = Rd::new(data);
    let pc = |v: i16| v as f32 / 100.0;
    Some(match key {
        b"grdm" => {
            let version = d.u16().ok()?;
            if version != 1 && version != 3 {
                return None;
            }
            let reverse = d.u8().ok()? != 0;
            d.u8().ok()?; // dithered
            if version == 3 {
                d.skip(4).ok()?; // interpolation method
            }
            let n = d.u32().ok()? as usize;
            d.skip(n.min(1 << 16) * 2).ok()?; // name
            let count = d.u16().ok()? as usize;
            let mut stops = Vec::with_capacity(count.min(256));
            for _ in 0..count {
                let loc = d.u32().ok()?;
                let _mid = d.u32().ok()?;
                let space = d.u16().ok()?;
                let c = [d.u16().ok()?, d.u16().ok()?, d.u16().ok()?, d.u16().ok()?];
                d.skip(2).ok()?;
                let color = match space {
                    0 => [linear_of16(c[0]), linear_of16(c[1]), linear_of16(c[2])],
                    // Greyscale (8): one component in 0..10000.
                    8 => [srgb_to_linear_f((c[0] as f32 / 10000.0).min(1.0)); 3],
                    // Other spaces (HSB, CMYK, Lab) are not decoded: grey.
                    _ => [srgb_to_linear_f(0.5); 3],
                };
                stops.push(GradientStop::new((loc as f32 / LOC).clamp(0.0, 1.0), color));
            }
            if stops.is_empty() {
                return None;
            }
            Adjustment::GradientMap {
                gradient: Gradient { stops },
                reverse,
            }
        }
        b"mixr" => {
            if d.u16().ok()? != 1 {
                return None;
            }
            let monochrome = d.u16().ok()? != 0;
            let mut rec = || -> Option<[f32; 4]> {
                let r = d.i16().ok()?;
                let g = d.i16().ok()?;
                let b = d.i16().ok()?;
                d.skip(2).ok()?;
                let k = d.i16().ok()?;
                Some([pc(r), pc(g), pc(b), pc(k)])
            };
            let first = rec()?;
            let ident = |i: usize| {
                let mut w = [0.0; 4];
                w[i] = 1.0;
                w
            };
            let (red, green, blue, gray) = if monochrome {
                let r = rec().unwrap_or(ident(0));
                let g = rec().unwrap_or(ident(1));
                let b = rec().unwrap_or(ident(2));
                // Zeroed colour rows (as some writers leave them) read
                // as the identity, so unticking Monochrome shows colour.
                let fix = |w: [f32; 4], i| if w == [0.0; 4] { ident(i) } else { w };
                (fix(r, 0), fix(g, 1), fix(b, 2), first)
            } else {
                let g = rec()?;
                let b = rec()?;
                let gray = rec().unwrap_or([0.4, 0.4, 0.2, 0.0]);
                (first, g, b, gray)
            };
            Adjustment::ChannelMixer {
                red,
                green,
                blue,
                monochrome,
                gray,
            }
        }
        b"phfl" => {
            let version = d.u16().ok()?;
            let color = match version {
                2 => {
                    let space = d.u16().ok()?;
                    let c = [d.u16().ok()?, d.u16().ok()?, d.u16().ok()?, d.u16().ok()?];
                    if space != 0 {
                        return None;
                    }
                    [linear_of16(c[0]), linear_of16(c[1]), linear_of16(c[2])]
                }
                // Version 3 stores L*a*b* (D50) × 100 as 32-bit integers.
                3 => {
                    let l = d.i32().ok()? as f32 / 100.0;
                    let a = d.i32().ok()? as f32 / 100.0;
                    let b = d.i32().ok()? as f32 / 100.0;
                    lab_to_linear_srgb(l, a, b)
                }
                _ => return None,
            };
            let density = (d.u32().ok()?.min(100) as f32) / 100.0;
            let preserve = d.u8().ok()? != 0;
            Adjustment::PhotoFilter {
                color,
                density,
                preserve_luminosity: preserve,
            }
        }
        b"selc" => {
            if d.u16().ok()? != 1 {
                return None;
            }
            let absolute = d.u16().ok()? == 1;
            d.skip(8).ok()?; // reserved record
            let mut colors = [[0f32; 4]; 9];
            for fam in colors.iter_mut() {
                for v in fam.iter_mut() {
                    *v = pc(d.i16().ok()?).clamp(-1.0, 1.0);
                }
            }
            Adjustment::SelectiveColor { colors, absolute }
        }
        _ => return None,
    })
}

/// CIE L*a*b* (D50, Photoshop's Lab) to straight linear sRGB, clipped.
fn lab_to_linear_srgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.964_22 * inv(fx), inv(fy), 0.825_21 * inv(fz));
    // Bradford-adapted XYZ(D50) → linear sRGB.
    let r = 3.133_856 * x - 1.616_867 * y - 0.490_615 * z;
    let g = -0.978_768 * x + 1.916_142 * y + 0.033_454 * z;
    let bl = 0.071_945 * x - 0.228_991 * y + 1.405_243 * z;
    [r, g, bl].map(|v| v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    fn round_trip(adj: &Adjustment) -> Adjustment {
        let (key, data) = adjustment_block(adj).expect("encodes");
        parse_adjustment(key, &data).expect("decodes")
    }

    #[test]
    fn gradient_maps_round_trip_with_exact_layout() {
        let adj = Adjustment::GradientMap {
            gradient: Gradient {
                stops: vec![
                    GradientStop::srgb8(0.0, [0, 0, 0]),
                    GradientStop::srgb8(0.25, [255, 0, 0]),
                    GradientStop::srgb8(1.0, [255, 255, 255]),
                ],
            },
            reverse: true,
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!(key, b"grdm");
        // version, reversed, dithered, name "Custom\0" (7 units), 3 stops.
        assert_eq!(&data[..4], &[0, 1, 1, 0]);
        assert_eq!(u32::from_be_bytes(data[4..8].try_into().unwrap()), 7);
        assert_eq!(u16::from_be_bytes(data[22..24].try_into().unwrap()), 3);
        // Second stop: location 1024 of 4096, midpoint 50, RGB, 65535/0/0.
        let s1 = &data[24 + 20..24 + 40];
        assert_eq!(u32::from_be_bytes(s1[0..4].try_into().unwrap()), 1024);
        assert_eq!(u32::from_be_bytes(s1[4..8].try_into().unwrap()), 50);
        assert_eq!(&s1[8..14], &[0, 0, 0xFF, 0xFF, 0, 0]);
        assert_eq!(data.len() % 4, 0);
        let back = round_trip(&adj);
        let Adjustment::GradientMap { gradient, reverse } = back else {
            panic!()
        };
        assert!(reverse);
        assert_eq!(gradient.stops.len(), 3);
        assert!(close(gradient.stops[1].pos, 0.25) && close(gradient.stops[1].color[0], 1.0));
        assert!(close(gradient.stops[2].color[1], 1.0) && close(gradient.stops[0].color[2], 0.0));
    }

    #[test]
    fn channel_mixers_round_trip_in_percent() {
        let adj = Adjustment::ChannelMixer {
            red: [1.2, -0.1, 0.0, 0.05],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.3, 0.7, -0.2],
            monochrome: false,
            gray: [0.4, 0.4, 0.2, 0.0],
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!((key, data.len()), (b"mixr", 44));
        // Red row: 120, -10, 0, (unused), 5.
        let row: Vec<i16> = data[4..14]
            .chunks(2)
            .map(|c| i16::from_be_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(row, vec![120, -10, 0, 0, 5]);
        assert_eq!(round_trip(&adj), adj);
        let mono = Adjustment::ChannelMixer {
            monochrome: true,
            gray: [-0.7, 2.0, -0.3, 0.0],
            red: [1.0, 0.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.0, 1.0, 0.0],
        };
        assert_eq!(round_trip(&mono), mono);
    }

    #[test]
    fn photo_filters_round_trip_and_read_lab_version_3() {
        let adj = Adjustment::PhotoFilter {
            color: [1.0, srgb_to_linear_f(138.0 / 255.0), 0.0],
            density: 0.25,
            preserve_luminosity: true,
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!(key, b"phfl");
        assert_eq!(&data[..4], &[0, 2, 0, 0]);
        assert_eq!(u16::from_be_bytes([data[6], data[7]]), 138 * 257);
        assert_eq!(u32::from_be_bytes(data[12..16].try_into().unwrap()), 25);
        let Adjustment::PhotoFilter {
            color,
            density,
            preserve_luminosity,
        } = round_trip(&adj)
        else {
            panic!()
        };
        assert!(
            close(color[1], srgb_to_linear_f(138.0 / 255.0)) && close(density, 0.25) && preserve_luminosity
        );

        // Version 3: L*a*b* white (100, 0, 0) is white.
        let mut v3 = vec![0, 3];
        for v in [10000i32, 0, 0] {
            v3.extend_from_slice(&v.to_be_bytes());
        }
        v3.extend_from_slice(&40u32.to_be_bytes());
        v3.push(0);
        let Some(Adjustment::PhotoFilter { color, density, .. }) = parse_adjustment(b"phfl", &v3) else {
            panic!()
        };
        assert!(color.iter().all(|c| close(*c, 1.0)), "{color:?}");
        assert!(close(density, 0.4));
    }

    /// Writes `<tmp>/lumenply-psd-extra/adjustments.psd` (checked with
    /// psd-tools as well) and reads it back through the full codec.
    #[test]
    fn a_psd_with_every_new_adjustment_round_trips() {
        use lumenply_doc::{Document, LayerContent};
        let mut doc = Document::new(8, 8);
        let bg = doc.add_pixel_layer("Background");
        doc.layer_mut(bg).unwrap().pixels_mut().unwrap().set_pixel(
            1,
            1,
            lumenply_tiles::Rgba::new(0.5, 0.2, 0.1, 1.0),
        );
        let mut sel = [[0f32; 4]; 9];
        sel[0] = [-0.3, 0.2, 0.6, 0.0];
        let adjs = vec![
            Adjustment::GradientMap {
                gradient: Gradient::presets()[1].1.clone(),
                reverse: false,
            },
            Adjustment::ChannelMixer {
                red: [1.0, 0.0, 0.0, 0.0],
                green: [0.0, 1.0, 0.0, 0.0],
                blue: [0.0, 0.0, 1.0, 0.0],
                monochrome: true,
                gray: [-0.7, 2.0, -0.3, 0.0],
            },
            Adjustment::photo_filter_default(),
            Adjustment::SelectiveColor {
                colors: sel,
                absolute: false,
            },
        ];
        for a in &adjs {
            doc.add_adjustment(a.clone());
        }
        let dir = std::env::temp_dir().join("lumenply-psd-extra");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adjustments.psd");
        let report = crate::psd::save(&path, &doc).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let back = crate::psd::load(&path).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let got: Vec<&Adjustment> = back
            .value
            .layers()
            .iter()
            .filter_map(|l| match &l.content {
                LayerContent::Adjustment(a) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(got.len(), 4);
        assert!(matches!(got[0], Adjustment::GradientMap { gradient, .. } if gradient.stops.len() == 3));
        assert_eq!(got[1], &adjs[1]);
        assert!(matches!(got[2], Adjustment::PhotoFilter { density, .. } if close(*density, 0.25)));
        assert_eq!(got[3], &adjs[3]);
    }

    #[test]
    fn selective_color_round_trips_every_family() {
        let mut colors = [[0f32; 4]; 9];
        colors[0] = [0.5, -0.25, 0.0, 0.1];
        colors[8] = [0.0, 0.0, 0.0, -1.0];
        let adj = Adjustment::SelectiveColor {
            colors,
            absolute: true,
        };
        let (key, data) = adjustment_block(&adj).unwrap();
        assert_eq!((key, data.len()), (b"selc", 4 + 80));
        assert_eq!(&data[..4], &[0, 1, 0, 1]);
        // Reds follow the reserved record: 50, -25, 0, 10.
        assert_eq!(&data[12..20], &[0, 50, 0xFF, 0xE7, 0, 0, 0, 10]);
        assert_eq!(round_trip(&adj), adj);
    }
}
