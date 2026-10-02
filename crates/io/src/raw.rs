//! Camera RAW import through rawler (the dnglab decoder): decode, scale to
//! the sensor's black/white levels, demosaic, white balance from the
//! camera, camera matrix to sRGB primaries, default crop. The developed
//! pixels are linear light — this engine's native space — so no gamma is
//! applied; the camera's orientation flag is honoured.

use crate::IoError;
use lumenply_tiles::{Raster, Rgba};
use std::path::Path;

/// File extensions opened as camera RAW.
pub const RAW_EXT: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "rwl",
    "3fr", "fff", "iiq", "mos", "mef", "mrw", "erf", "kdc", "dcr", "raw",
];

/// Whether `path` names a camera RAW file (by extension).
pub fn is_raw(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RAW_EXT.iter().any(|r| r.eq_ignore_ascii_case(e)))
}

/// Decode and develop a camera RAW file into linear, premultiplied pixels.
pub fn load_raw(path: impl AsRef<Path>) -> Result<Raster, IoError> {
    use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
    let raw = rawler::decode_file(path.as_ref()).map_err(|e| IoError::Codec(format!("camera RAW: {e}")))?;
    let develop = RawDevelop {
        steps: RawDevelop::default()
            .steps
            .into_iter()
            .filter(|s| *s != ProcessingStep::SRgb)
            .collect(),
    };
    let developed = develop
        .develop_intermediate(&raw)
        .map_err(|e| IoError::Codec(format!("camera RAW: {e}")))?;
    let (w, h, rgb): (usize, usize, Vec<[f32; 3]>) = match developed {
        Intermediate::ThreeColor(px) => (px.width, px.height, px.data),
        Intermediate::Monochrome(px) => (px.width, px.height, px.data.iter().map(|&v| [v, v, v]).collect()),
        Intermediate::FourColor(px) => (
            px.width,
            px.height,
            px.data.iter().map(|v| [v[0], v[1], v[2]]).collect(),
        ),
    };
    if w == 0 || h == 0 || rgb.len() != w * h {
        return Err(IoError::Codec("camera RAW: empty image".into()));
    }
    Ok(orient(&to_raster(w, h, &rgb), orientation_code(raw.orientation)))
}

/// Developed linear RGB to opaque pixels; negative lobes from the colour
/// matrix are clamped to zero.
fn to_raster(w: usize, h: usize, rgb: &[[f32; 3]]) -> Raster {
    let mut out = Raster::new(w as u32, h as u32);
    for (p, c) in out.pixels.iter_mut().zip(rgb) {
        *p = Rgba::new(c[0].max(0.0), c[1].max(0.0), c[2].max(0.0), 1.0);
    }
    out
}

/// The EXIF orientation number (1-8) for rawler's orientation.
fn orientation_code(o: rawler::Orientation) -> u8 {
    use rawler::Orientation as O;
    match o {
        O::Normal | O::Unknown => 1,
        O::HorizontalFlip => 2,
        O::Rotate180 => 3,
        O::VerticalFlip => 4,
        O::Transpose => 5,
        O::Rotate90 => 6,
        O::Transverse => 7,
        O::Rotate270 => 8,
    }
}

/// Applies EXIF orientation `code` so the image displays upright.
fn orient(src: &Raster, code: u8) -> Raster {
    let (w, h) = (src.width, src.height);
    let swap = code >= 5;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let mut out = Raster::new(ow, oh);
    for y in 0..oh {
        for x in 0..ow {
            // The source pixel shown at output (x, y).
            let (sx, sy) = match code {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                8 => (w - 1 - y, x),
                _ => (x, y),
            };
            out.set(x, y, src.get(sx, sy));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(v: f32) -> Rgba {
        Rgba::new(v, v, v, 1.0)
    }

    /// A 3×2 raster whose pixels number 0..6 row by row.
    fn numbered() -> Raster {
        let mut r = Raster::new(3, 2);
        for i in 0..6 {
            r.pixels[i] = px(i as f32);
        }
        r
    }

    fn values(r: &Raster) -> Vec<f32> {
        r.pixels.iter().map(|p| p.r).collect()
    }

    #[test]
    fn every_exif_orientation_lands_upright() {
        // 0 1 2
        // 3 4 5
        let r = numbered();
        assert_eq!(values(&orient(&r, 1)), [0., 1., 2., 3., 4., 5.]);
        assert_eq!(values(&orient(&r, 2)), [2., 1., 0., 5., 4., 3.]);
        assert_eq!(values(&orient(&r, 3)), [5., 4., 3., 2., 1., 0.]);
        assert_eq!(values(&orient(&r, 4)), [3., 4., 5., 0., 1., 2.]);
        // Rotated cases are 2 wide, 3 tall.
        let r6 = orient(&r, 6); // 90° clockwise
        assert_eq!((r6.width, r6.height), (2, 3));
        assert_eq!(values(&r6), [3., 0., 4., 1., 5., 2.]);
        assert_eq!(values(&orient(&r, 8)), [2., 5., 1., 4., 0., 3.]); // 90° counter-clockwise
        assert_eq!(values(&orient(&r, 5)), [0., 3., 1., 4., 2., 5.]); // transpose
        assert_eq!(values(&orient(&r, 7)), [5., 2., 4., 1., 3., 0.]); // transverse
    }

    #[test]
    fn developed_values_become_opaque_linear_pixels() {
        let r = to_raster(2, 1, &[[0.25, 0.5, 1.0], [-0.1, 0.0, 0.75]]);
        assert_eq!(r.get(0, 0), Rgba::new(0.25, 0.5, 1.0, 1.0));
        assert_eq!(
            r.get(1, 0),
            Rgba::new(0.0, 0.0, 0.75, 1.0),
            "negative lobes clamp to zero"
        );
    }

    #[test]
    fn raw_files_are_recognised_by_extension() {
        assert!(is_raw("IMG_0001.CR3") && is_raw("a.nef") && is_raw("x/y.dng") && is_raw("p.ARW"));
        assert!(!is_raw("photo.jpg") && !is_raw("raw") && !is_raw("a.lumen"));
    }

    #[test]
    fn a_non_raw_file_fails_cleanly() {
        let p = std::env::temp_dir().join(format!("lumenply-not-raw-{}.nef", std::process::id()));
        std::fs::write(&p, b"definitely not a camera file").unwrap();
        let err = load_raw(&p).unwrap_err();
        let _ = std::fs::remove_file(&p);
        assert!(err.to_string().contains("camera RAW"), "{err}");
    }
}
