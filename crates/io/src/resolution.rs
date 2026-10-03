//! Print resolution (pixels per inch) in image files.
//!
//! Each format keeps it differently:
//! - PSD: image resource 1005 (ResolutionInfo): horizontal resolution as
//!   16.16 fixed point, *always* in pixels per inch (the unit fields only
//!   say how Photoshop displays it: 1 = ppi, 2 = px/cm), then the width
//!   display unit, then the same three for vertical.
//! - PNG: the `pHYs` chunk, pixels per metre with unit byte 1.
//! - JPEG: the JFIF APP0 density (units 1 = per inch, 2 = per cm), or the
//!   EXIF/TIFF X resolution; a Photoshop APP13 block's 1005 wins over both.
//! - TIFF: XResolution (a rational) with ResolutionUnit (2 inch, 3 cm).
//!
//! Writers here patch already-encoded bytes, so the codecs in `lib.rs`
//! stay resolution-free. Readers never fail: a missing or malformed field
//! is `None`, and the caller falls back to 72 ppi.

use std::path::Path;

use lumenply_doc::RESOLUTION_RANGE;
use lumenply_tiles::Raster;

use crate::IoError;

/// Photoshop's ResolutionInfo image resource.
pub(crate) const RESOLUTION_INFO: u16 = 1005;

const METRES_PER_INCH: f64 = 0.0254;

/// A resolution read from a file, cleaned up: quantisation noise from the
/// storage format (299.9994 from PNG's pixels per metre, 71.99998 from
/// 16.16 fixed point) snaps to the whole number it came from, anything
/// else keeps two decimals. `None` when not a usable resolution.
pub fn snap_ppi(ppi: f64) -> Option<f32> {
    if !ppi.is_finite() {
        return None;
    }
    let whole = ppi.round();
    let v = if (ppi - whole).abs() < 0.02 {
        whole
    } else {
        (ppi * 100.0).round() / 100.0
    };
    let v = v as f32;
    RESOLUTION_RANGE.contains(&v).then_some(v)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

// ---- PSD ------------------------------------------------------------------------

/// The `8BIM` 1005 block for `ppi` (unit ppi, sizes in inches), ready to
/// append to an image-resources section body.
pub(crate) fn psd_resource_block(ppi: f32) -> Vec<u8> {
    let fixed = (ppi as f64 * 65536.0).round().clamp(1.0, u32::MAX as f64) as u32;
    let mut data = Vec::with_capacity(16);
    for _ in 0..2 {
        data.extend_from_slice(&fixed.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes()); // shown as pixels per inch
        data.extend_from_slice(&1u16.to_be_bytes()); // sizes shown in inches
    }
    let mut block = Vec::with_capacity(28);
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(&RESOLUTION_INFO.to_be_bytes());
    block.extend_from_slice(&[0, 0]); // empty Pascal name, padded to even
    block.extend_from_slice(&(data.len() as u32).to_be_bytes());
    block.extend_from_slice(&data);
    block
}

/// Each `(id, data)` in an image-resources section body. Stops at the first
/// malformed block.
fn psd_blocks(res: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        if res.get(at..at + 4)? != b"8BIM" {
            return None;
        }
        let id = be16(res, at + 4)?;
        // Pascal name: length byte + bytes, padded to an even total.
        let name_total = 1 + *res.get(at + 6)? as usize;
        let p = at + 6 + name_total + (name_total % 2);
        let size = be32(res, p)? as usize;
        let data = res.get(p + 4..(p + 4).checked_add(size)?)?;
        at = p + 4 + size + (size % 2);
        Some((id, data))
    })
}

/// The horizontal resolution from an image-resources section body.
pub(crate) fn read_psd(res: &[u8]) -> Option<f32> {
    let (_, data) = psd_blocks(res).find(|(id, _)| *id == RESOLUTION_INFO)?;
    if data.len() < 16 {
        return None;
    }
    snap_ppi(be32(data, 0)? as f64 / 65536.0)
}

// ---- PNG ------------------------------------------------------------------------

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Each `(type, data, whole chunk)` of a PNG stream, up to IEND.
fn png_chunks(png: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8], &[u8])> {
    let mut at = PNG_SIG.len();
    std::iter::from_fn(move || {
        let len = be32(png, at)? as usize;
        let kind: [u8; 4] = png.get(at + 4..at + 8)?.try_into().ok()?;
        let end = (at + 12).checked_add(len)?;
        let whole = png.get(at..end)?;
        at = end;
        Some((kind, &whole[8..8 + len], whole))
    })
}

/// Resolution from a PNG's `pHYs` chunk (unit must be the metre).
pub fn png_ppi(png: &[u8]) -> Option<f32> {
    if !png.starts_with(PNG_SIG) {
        return None;
    }
    let (_, data, _) = png_chunks(png)
        .take_while(|(k, _, _)| k != b"IDAT")
        .find(|(k, _, _)| k == b"pHYs")?;
    if data.len() < 9 || data[8] != 1 {
        return None;
    }
    snap_ppi(be32(data, 0)? as f64 * METRES_PER_INCH)
}

/// `png` with its `pHYs` chunk set to `ppi` (inserted after IHDR, replacing
/// any existing one). Bytes that aren't a well-formed PNG come back as is.
pub fn png_with_ppi(png: &[u8], ppi: f32) -> Vec<u8> {
    if !png.starts_with(PNG_SIG) || png_chunks(png).next().map(|c| c.0) != Some(*b"IHDR") {
        return png.to_vec();
    }
    let ppm = (ppi as f64 / METRES_PER_INCH).round().clamp(1.0, u32::MAX as f64) as u32;
    let mut body = Vec::with_capacity(13);
    body.extend_from_slice(b"pHYs");
    body.extend_from_slice(&ppm.to_be_bytes());
    body.extend_from_slice(&ppm.to_be_bytes());
    body.push(1); // unit: the metre
    let mut crc = flate2::Crc::new();
    crc.update(&body);
    let mut out = Vec::with_capacity(png.len() + 21);
    out.extend_from_slice(PNG_SIG);
    let mut consumed = PNG_SIG.len();
    for (kind, _, whole) in png_chunks(png) {
        consumed += whole.len();
        if &kind == b"pHYs" {
            continue;
        }
        out.extend_from_slice(whole);
        if &kind == b"IHDR" {
            out.extend_from_slice(&9u32.to_be_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc.sum().to_be_bytes());
        }
    }
    // Anything after the last whole chunk (trailing bytes) is kept.
    out.extend_from_slice(&png[consumed.min(png.len())..]);
    out
}

// ---- TIFF / EXIF ----------------------------------------------------------------

/// Resolution from a TIFF stream's first IFD (also the layout inside an
/// EXIF block). Unit 1 ("none") gives `None`.
pub fn tiff_ppi(t: &[u8]) -> Option<f32> {
    let le = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = t.get(at..at + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = t.get(at..at + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    let (mut x, mut unit) = (None, 2u16);
    for i in 0..count {
        let e = ifd + 2 + 12 * i;
        let (tag, kind) = (u16_at(e)?, u16_at(e + 2)?);
        match (tag, kind) {
            // XResolution, RATIONAL: numerator and denominator at an offset.
            (282, 5) => {
                let off = u32_at(e + 8)? as usize;
                let (n, d) = (u32_at(off)?, u32_at(off + 4)?);
                if d != 0 {
                    x = Some(n as f64 / d as f64);
                }
            }
            // ResolutionUnit, SHORT stored in the value field.
            (296, 3) => unit = u16_at(e + 8)?,
            _ => {}
        }
    }
    match unit {
        2 => snap_ppi(x?),
        3 => snap_ppi(x? * 2.54),
        _ => None,
    }
}

// ---- JPEG -----------------------------------------------------------------------

/// Each `(marker, payload)` of a JPEG's header segments, up to the scan.
fn jpeg_segments(j: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 2usize;
    let ok = j.starts_with(&[0xFF, 0xD8]);
    std::iter::from_fn(move || {
        if !ok || *j.get(at)? != 0xFF {
            return None;
        }
        let marker = *j.get(at + 1)?;
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = be16(j, at + 2)? as usize;
        let payload = j.get(at + 4..(at + 2).checked_add(len)?)?;
        at += 2 + len;
        Some((marker, payload))
    })
}

/// Resolution from a JPEG: a Photoshop APP13 resource 1005, else EXIF's
/// X resolution, else the JFIF density.
pub fn jpeg_ppi(j: &[u8]) -> Option<f32> {
    let (mut psd, mut exif, mut jfif) = (None, None, None);
    for (marker, p) in jpeg_segments(j) {
        match marker {
            0xE0 if p.starts_with(b"JFIF\0") && p.len() >= 12 => {
                let d = be16(p, 8)? as f64;
                jfif = match p[7] {
                    1 => snap_ppi(d),
                    2 => snap_ppi(d * 2.54),
                    _ => None,
                };
            }
            0xE1 if p.starts_with(b"Exif\0\0") => exif = tiff_ppi(&p[6..]),
            0xED if p.starts_with(b"Photoshop 3.0\0") => psd = read_psd(&p[14..]),
            _ => {}
        }
    }
    psd.or(exif).or(jfif)
}

/// `jpeg` with its JFIF density set to `ppi` (whole pixels per inch),
/// adding a JFIF header when there is none. Other bytes come back as is.
pub fn jpeg_with_ppi(jpeg: &[u8], ppi: f32) -> Vec<u8> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return jpeg.to_vec();
    }
    let d = (ppi.round() as u32).clamp(1, 0xFFFF) as u16;
    let mut out = jpeg.to_vec();
    if let Some((0xE0, p)) = jpeg_segments(jpeg).next() {
        if p.starts_with(b"JFIF\0") && p.len() >= 12 {
            // APP0 starts at 2: marker (2), length (2), then the payload,
            // whose byte 7 is the unit.
            let at = 6 + 7;
            out[at] = 1;
            out[at + 1..at + 3].copy_from_slice(&d.to_be_bytes());
            out[at + 3..at + 5].copy_from_slice(&d.to_be_bytes());
            return out;
        }
    }
    let mut app0 = vec![0xFF, 0xE0, 0x00, 0x10];
    app0.extend_from_slice(b"JFIF\0");
    app0.extend_from_slice(&[1, 1, 1]); // version 1.1, units: per inch
    app0.extend_from_slice(&d.to_be_bytes());
    app0.extend_from_slice(&d.to_be_bytes());
    app0.extend_from_slice(&[0, 0]); // no thumbnail
    out.splice(2..2, app0);
    out
}

// ---- any format -----------------------------------------------------------------

/// The resolution stored in an encoded PNG, JPEG or TIFF; `None` for other
/// formats or when the file has none.
pub fn read_ppi(bytes: &[u8]) -> Option<f32> {
    if bytes.starts_with(PNG_SIG) {
        png_ppi(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg_ppi(bytes)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        tiff_ppi(bytes)
    } else {
        None
    }
}

/// `bytes` with `ppi` written in when the format is PNG or JPEG; other
/// formats (WebP, GIF, …) have no common resolution field and come back
/// unchanged.
pub fn with_ppi(bytes: Vec<u8>, ppi: f32) -> Vec<u8> {
    if bytes.starts_with(PNG_SIG) {
        png_with_ppi(&bytes, ppi)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg_with_ppi(&bytes, ppi)
    } else {
        bytes
    }
}

/// The resolution of the image file at `path` (see [`read_ppi`]).
pub fn file_ppi(path: impl AsRef<Path>) -> Option<f32> {
    read_ppi(&std::fs::read(path).ok()?)
}

/// Write `ppi` into the PNG or JPEG file at `path` in place.
pub fn set_file_ppi(path: impl AsRef<Path>, ppi: f32) -> Result<(), IoError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let out = with_ppi(bytes.clone(), ppi);
    if out != bytes {
        std::fs::write(path, out)?;
    }
    Ok(())
}

/// [`crate::save_png`] with the resolution written in.
pub fn save_png(path: impl AsRef<Path>, raster: &Raster, ppi: f32) -> Result<(), IoError> {
    std::fs::write(path, png_with_ppi(&crate::encode_png(raster, true)?, ppi))?;
    Ok(())
}

/// [`crate::save_jpeg`] with the resolution written in.
pub fn save_jpeg(path: impl AsRef<Path>, raster: &Raster, quality: u8, ppi: f32) -> Result<(), IoError> {
    std::fs::write(path, jpeg_with_ppi(&crate::encode_jpeg(raster, quality)?, ppi))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_tiles::Rgba;

    fn sample() -> Raster {
        let mut r = Raster::new(7, 5);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            *p = Rgba::from_straight((i % 7) as f32 / 7.0, 0.3, 0.6, 1.0);
        }
        r
    }

    #[test]
    fn snapping() {
        assert_eq!(snap_ppi(299.9994), Some(300.0));
        assert_eq!(snap_ppi(4718591.0 / 65536.0), Some(72.0));
        assert_eq!(snap_ppi(118.11 * 2.54), Some(300.0));
        assert_eq!(snap_ppi(72.5), Some(72.5));
        assert_eq!(snap_ppi(96.123), Some(96.12));
        assert_eq!(snap_ppi(0.0), None);
        assert_eq!(snap_ppi(f64::NAN), None);
        assert_eq!(snap_ppi(1e9), None);
    }

    #[test]
    fn psd_block_round_trip() {
        let block = psd_resource_block(300.0);
        assert_eq!(block.len(), 12 + 16);
        // 300 · 65536 = 19660800, the value Photoshop writes for 300 ppi.
        assert_eq!(be32(&block, 12), Some(19_660_800));
        assert_eq!(be16(&block, 16), Some(1));
        assert_eq!(read_psd(&block), Some(300.0));
        // Found after another block (a guides block with an odd name).
        let mut res = b"8BIM\x04\x08\x03abc\0\0\0\x01\x07\0".to_vec();
        res.extend_from_slice(&psd_resource_block(72.0));
        assert_eq!(read_psd(&res), Some(72.0));
        assert_eq!(
            read_psd(b"8BIM\x03\xed\0\0\0\0\0\x04abcd"),
            None,
            "truncated data"
        );
        assert_eq!(read_psd(&[]), None);
    }

    #[test]
    fn png_phys_round_trip() {
        let png = crate::encode_png(&sample(), true).unwrap();
        assert_eq!(png_ppi(&png), None, "the encoder writes no pHYs");
        let tagged = png_with_ppi(&png, 300.0);
        assert_eq!(tagged.len(), png.len() + 21);
        // 300 ppi = 11811 px/m (300 / 0.0254 = 11811.02).
        let at = tagged.windows(4).position(|w| w == b"pHYs").unwrap();
        assert_eq!(be32(&tagged, at + 4), Some(11811));
        assert_eq!(png_ppi(&tagged), Some(300.0));
        // Re-tagging replaces rather than adds.
        let again = png_with_ppi(&tagged, 150.0);
        assert_eq!(again.len(), tagged.len());
        assert_eq!(png_ppi(&again), Some(150.0));
        assert_eq!(again.windows(4).filter(|w| *w == b"pHYs").count(), 1);
        // Still a valid PNG (CRCs checked by the decoder) with the same pixels.
        let back = image::load_from_memory(&again).unwrap().to_rgba8();
        let orig = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(back.as_raw(), orig.as_raw());
        // The png crate reads our chunk the same way.
        let dec = png::Decoder::new(std::io::Cursor::new(&again))
            .read_info()
            .unwrap();
        let phys = dec.info().pixel_dims.unwrap();
        assert_eq!((phys.xppu, phys.yppu, phys.unit), (5906, 5906, png::Unit::Meter));
        assert_eq!(read_ppi(&again), Some(150.0));
    }

    #[test]
    fn jpeg_density_round_trip() {
        let jpg = crate::encode_jpeg(&sample(), 90).unwrap();
        let tagged = jpeg_with_ppi(&jpg, 300.0);
        assert_eq!(jpeg_ppi(&tagged), Some(300.0));
        assert_eq!(read_ppi(&tagged), Some(300.0));
        assert!(image::load_from_memory(&tagged).is_ok());
        // Without a JFIF header one is added right after SOI.
        let mut bare = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x00, 0x04, b'h', b'i', 0xFF, 0xD9];
        bare = jpeg_with_ppi(&bare, 240.4);
        assert_eq!(&bare[2..4], &[0xFF, 0xE0]);
        assert_eq!(jpeg_ppi(&bare), Some(240.0));
        // Density in dots per cm: 118 px/cm ≈ 299.72 ppi.
        let mut cm = tagged.clone();
        cm[13] = 2;
        cm[14..16].copy_from_slice(&118u16.to_be_bytes());
        assert_eq!(jpeg_ppi(&cm), Some(299.72));
        // Units 0 (aspect ratio only) carry no resolution.
        cm[13] = 0;
        assert_eq!(jpeg_ppi(&cm), None);
    }

    /// A little-endian TIFF header + IFD with XResolution `n/d` and
    /// ResolutionUnit `unit`, as cameras put inside EXIF.
    fn tiff_bytes(n: u32, d: u32, unit: u16) -> Vec<u8> {
        let mut t = b"II*\0".to_vec();
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        // 282 RATIONAL count 1 at offset 8 + 2 + 24 + 4 = 38.
        t.extend_from_slice(&282u16.to_le_bytes());
        t.extend_from_slice(&5u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        t.extend_from_slice(&296u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&(unit as u32).to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&d.to_le_bytes());
        t
    }

    #[test]
    fn tiff_and_exif() {
        assert_eq!(tiff_ppi(&tiff_bytes(350, 1, 2)), Some(350.0));
        assert_eq!(tiff_ppi(&tiff_bytes(30000, 254, 3)), Some(300.0), "118.11 px/cm");
        assert_eq!(tiff_ppi(&tiff_bytes(72, 1, 1)), None, "unit none");
        assert_eq!(read_ppi(&tiff_bytes(600, 2, 2)), Some(300.0));
        // EXIF beats the JFIF density.
        let jpg = jpeg_with_ppi(&crate::encode_jpeg(&sample(), 80).unwrap(), 72.0);
        let mut app1 = vec![0xFF, 0xE1];
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff_bytes(350, 1, 2));
        app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        app1.extend_from_slice(&payload);
        let mut with_exif = jpg.clone();
        with_exif.splice(20..20, app1);
        assert_eq!(jpeg_ppi(&with_exif), Some(350.0));
    }

    #[test]
    fn project_and_psd_round_trip() {
        let dir = std::env::temp_dir().join("lumenply-resolution-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut doc = lumenply_doc::Document::new(30, 20);
        doc.add_pixel_layer("p");
        doc.resolution = 300.0;

        let lumen = dir.join("r.lumen");
        crate::project::save(&lumen, &doc).unwrap();
        assert_eq!(crate::project::load(&lumen).unwrap().resolution, 300.0);
        doc.resolution = 96.5;
        crate::project::save(&lumen, &doc).unwrap();
        assert_eq!(crate::project::load(&lumen).unwrap().resolution, 96.5);

        doc.resolution = 300.0;
        let psd = dir.join("r.psd");
        crate::psd::save(&psd, &doc).unwrap();
        assert_eq!(crate::psd::load(&psd).unwrap().value.resolution, 300.0);
        // The block sits in the file as Photoshop writes it: 300 · 65536.
        let bytes = std::fs::read(&psd).unwrap();
        let at = bytes.windows(6).position(|w| w == b"8BIM\x03\xed").unwrap();
        assert_eq!(be32(&bytes, at + 12), Some(19_660_800));
        crate::psd::save_16(&psd, &doc).unwrap();
        assert_eq!(crate::psd::load(&psd).unwrap().value.resolution, 300.0);
    }

    #[test]
    fn ora_round_trip() {
        let dir = std::env::temp_dir().join("lumenply-resolution-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut doc = lumenply_doc::Document::new(12, 8);
        doc.add_pixel_layer("p");
        doc.resolution = 240.0;
        let ora = dir.join("r.ora");
        crate::ora::save(&ora, &doc).unwrap();
        assert_eq!(crate::ora::load(&ora).unwrap().value.resolution, 240.0);
        doc.resolution = 72.5;
        crate::ora::save(&ora, &doc).unwrap();
        assert_eq!(crate::ora::load(&ora).unwrap().value.resolution, 72.5);
    }

    #[test]
    fn old_projects_load_as_72_ppi() {
        // A manifest written before the field existed: rewrite one without it.
        let dir = std::env::temp_dir().join("lumenply-resolution-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut doc = lumenply_doc::Document::new(8, 8);
        doc.resolution = 300.0;
        let src = dir.join("new.lumen");
        crate::project::save(&src, &doc).unwrap();
        let mut zin = zip::ZipArchive::new(std::fs::File::open(&src).unwrap()).unwrap();
        let old = dir.join("old.lumen");
        let mut zout = zip::ZipWriter::new(std::fs::File::create(&old).unwrap());
        for i in 0..zin.len() {
            let mut f = zin.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            if name == "manifest.json" {
                let mut v: serde_json::Value = serde_json::from_slice(&data).unwrap();
                assert_eq!(v["resolution"], 300.0);
                v.as_object_mut().unwrap().remove("resolution");
                data = serde_json::to_vec(&v).unwrap();
            }
            zout.start_file(name, zip::write::FileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zout, &data).unwrap();
        }
        zout.finish().unwrap();
        assert_eq!(crate::project::load(&old).unwrap().resolution, 72.0);
    }

    #[test]
    fn files_on_disk() {
        let dir = std::env::temp_dir().join("lumenply-resolution-test");
        std::fs::create_dir_all(&dir).unwrap();
        let (png, jpg) = (dir.join("a.png"), dir.join("a.jpg"));
        save_png(&png, &sample(), 300.0).unwrap();
        save_jpeg(&jpg, &sample(), 90, 200.0).unwrap();
        assert_eq!(file_ppi(&png), Some(300.0));
        assert_eq!(file_ppi(&jpg), Some(200.0));
        set_file_ppi(&png, 96.0).unwrap();
        assert_eq!(file_ppi(&png), Some(96.0));
        assert_eq!(crate::load(&png).unwrap().width, 7);
        assert_eq!(file_ppi(dir.join("missing.png")), None);
    }
}
