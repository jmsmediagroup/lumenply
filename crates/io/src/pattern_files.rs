//! Pattern pixels on disk (ADR 0016): 16-bit sRGB PNGs with straight alpha,
//! used by `.lumen` projects (`patterns/<id>.png`) and the app's pattern
//! library, plus Photoshop `.pat` preset files.

use lumenply_doc::Pattern;
use lumenply_tiles::{Raster, Rgba};

use crate::{linear_to_srgb_f, srgb_to_linear_f, IoError};

/// A pattern's pixels as a 16-bit PNG.
pub fn encode_pattern_png(r: &Raster) -> Result<Vec<u8>, IoError> {
    let mut bytes = Vec::with_capacity(r.pixels.len() * 8);
    for p in &r.pixels {
        let [cr, cg, cb, a] = p.to_straight();
        let q = |v: f32| (linear_to_srgb_f(v.clamp(0.0, 1.0)) * 65535.0 + 0.5) as u16;
        for v in [q(cr), q(cg), q(cb), (a.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
    }
    let mut out = Vec::new();
    crate::png_srgb_into(&mut out, r.width, r.height, png::BitDepth::Sixteen, &bytes)?;
    Ok(out)
}

/// Pattern pixels from PNG (or any image) bytes.
pub fn decode_pattern_png(bytes: &[u8]) -> Result<Raster, IoError> {
    let img = image::load_from_memory(bytes).map_err(|e| IoError::Codec(e.to_string()))?;
    let img = img.to_rgba16();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(IoError::Codec(format!(
            "pattern image {w}×{h} is not a sane size"
        )));
    }
    let mut out = Raster::new(w, h);
    for (i, px) in img.pixels().enumerate() {
        let c = |v: u16| srgb_to_linear_f(v as f32 / 65535.0);
        out.pixels[i] = Rgba::from_straight(c(px[0]), c(px[1]), c(px[2]), px[3] as f32 / 65535.0);
    }
    Ok(out)
}

/// A file-name-safe form of a pattern id.
pub fn file_stem(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if s.is_empty() {
        "pattern".into()
    } else {
        s
    }
}

/// Read a Photoshop `.pat` preset file (`8BPT`, version 1, then the same
/// per-pattern records as a PSD's `Patt` block).
pub fn load_pat(bytes: &[u8]) -> Result<Vec<Pattern>, IoError> {
    if bytes.len() < 10 || &bytes[..4] != b"8BPT" {
        return Err(IoError::Codec("not a Photoshop pattern file".into()));
    }
    let count = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
    let mut at = 10;
    let mut out = Vec::new();
    for _ in 0..count.min(10_000) {
        match crate::psd::read_pattern_record_pub(&bytes[at..]) {
            Some((p, used)) => {
                out.push(p);
                at += used;
            }
            None => break,
        }
    }
    if out.is_empty() && count > 0 {
        return Err(IoError::Codec("no readable pattern in the file".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_keeps_16_bit_precision_and_alpha() {
        let mut r = Raster::new(3, 2);
        r.set(0, 0, Rgba::new(0.2, 0.4, 0.6, 1.0));
        r.set(2, 1, Rgba::from_straight(0.5, 0.25, 0.125, 0.5));
        let back = decode_pattern_png(&encode_pattern_png(&r).unwrap()).unwrap();
        assert_eq!((back.width, back.height), (3, 2));
        for (a, b) in r.pixels.iter().zip(&back.pixels) {
            assert!(
                (a.r - b.r).abs() < 1e-4 && (a.a - b.a).abs() < 1e-4,
                "{a:?} {b:?}"
            );
        }
        assert_eq!(back.get(1, 0).a, 0.0);
    }

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(file_stem("b2fdfd29-de85-11d5"), "b2fdfd29-de85-11d5");
        assert_eq!(file_stem("../a b"), "___a_b");
        assert_eq!(file_stem(""), "pattern");
    }
}
