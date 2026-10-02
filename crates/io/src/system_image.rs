//! Formats the operating system decodes for us: HEIC / HEIF (iPhone and
//! many cameras) and AVIF. On macOS ImageIO reads them, applies the
//! orientation and converts their colour profile (Display P3, HDR gain
//! maps aside) while drawing into a linear-sRGB float bitmap with
//! premultiplied alpha, which is exactly Lumenply's pixel format.
//! Elsewhere they fail with a clear message.

use std::path::Path;

use lumenply_tiles::Raster;

use crate::IoError;

/// Extensions handed to the system decoder.
pub const SYSTEM_EXT: &[&str] = &["heic", "heif", "hif", "avif"];

/// Whether `path` names a format only the system decoder reads.
pub fn is_system_format(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SYSTEM_EXT.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[cfg(target_os = "macos")]
pub fn load_system(path: &Path) -> Result<Raster, IoError> {
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, CGPoint, CGRect, CGSize, CFURL};
    use objc2_core_graphics::{
        kCGColorSpaceLinearSRGB, CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
        CGImageByteOrderInfo, CGImageComponentInfo,
    };
    use objc2_image_io::{
        kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
        CGImageSource,
    };

    let fail = |what: &str| IoError::Codec(format!("{}: {what}", path.display()));
    let url = CFURL::from_file_path(path).ok_or_else(|| fail("not a file path"))?;
    // SAFETY: no options; the URL is a live file URL.
    let source =
        unsafe { CGImageSource::with_url(&url, None) }.ok_or_else(|| fail("the system can't read it"))?;
    // A full-size "thumbnail" built from the image with its orientation
    // applied (no size limit is given, so none is imposed).
    // SAFETY: the keys are ImageIO's own constants.
    let keys: [&CFString; 2] = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
        ]
    };
    let yes = CFBoolean::new(true);
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&keys, &[yes, yes]);
    // SAFETY: the options dictionary maps ImageIO keys to booleans.
    let image = unsafe { source.thumbnail_at_index(0, Some(options.as_opaque())) }
        .ok_or_else(|| fail("no image inside"))?;
    let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    if w == 0 || h == 0 || w > 65_535 || h > 65_535 {
        return Err(fail("unusable image size"));
    }
    // SAFETY: a system colour space name.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceLinearSRGB }))
        .ok_or_else(|| fail("no linear sRGB colour space"))?;
    let mut buf = vec![0f32; w * h * 4];
    let info = CGImageAlphaInfo::PremultipliedLast.0
        | CGImageComponentInfo::Float.0
        | CGImageByteOrderInfo::Order32Little.0;
    {
        // SAFETY: `buf` holds w × h RGBA f32 pixels (16 bytes each) and
        // outlives the context, which is dropped at the end of this block.
        let ctx =
            unsafe { CGBitmapContextCreate(buf.as_mut_ptr().cast(), w, h, 32, w * 16, Some(&space), info) }
                .ok_or_else(|| fail("can't make a bitmap"))?;
        let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
        CGContext::draw_image(Some(&ctx), rect, Some(&image));
    }
    let mut out = Raster::new(w as u32, h as u32);
    for (p, c) in out.pixels.iter_mut().zip(buf.chunks_exact(4)) {
        let a = c[3].clamp(0.0, 1.0);
        // Premultiplied already; clip wide-gamut excursions into range.
        *p = lumenply_tiles::Rgba::new(c[0].clamp(0.0, a), c[1].clamp(0.0, a), c[2].clamp(0.0, a), a);
    }
    Ok(out)
}

#[cfg(not(target_os = "macos"))]
pub fn load_system(path: &Path) -> Result<Raster, IoError> {
    Err(IoError::Codec(format!(
        "{}: HEIC/HEIF/AVIF needs the macOS system decoder; convert it to PNG, JPEG or TIFF first",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_formats_are_recognised_by_extension() {
        assert!(is_system_format(Path::new("/a/IMG_0001.HEIC")));
        assert!(is_system_format(Path::new("b.avif")));
        assert!(!is_system_format(Path::new("c.png")));
        assert!(!is_system_format(Path::new("heic")));
    }

    /// Encodes a known image to HEIC with the system's own `sips` and reads
    /// it back: orientation, size and colour survive.
    #[cfg(target_os = "macos")]
    #[test]
    fn heic_written_by_the_system_reads_back() {
        let dir = std::env::temp_dir().join(format!("lumenply-heic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("in.png");
        let heic = dir.join("out.heic");
        // 64×32: left half red, right half mid grey.
        let mut r = Raster::new(64, 32);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            *p = if i % 64 < 32 {
                lumenply_tiles::Rgba::new(1.0, 0.0, 0.0, 1.0)
            } else {
                let g = crate::srgb_to_linear(128);
                lumenply_tiles::Rgba::new(g, g, g, 1.0)
            };
        }
        crate::save_png(&png, &r).unwrap();
        let ok = std::process::Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(&png)
            .arg("--out")
            .arg(&heic)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("sips can't write HEIC here; skipping");
            return;
        }
        let back = load_system(&heic).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((back.width, back.height), (64, 32));
        let red = back.get(8, 16).to_straight();
        assert!(red[0] > 0.9 && red[1] < 0.05 && red[3] == 1.0, "{red:?}");
        let grey = back.get(56, 16).to_straight();
        let want = crate::srgb_to_linear(128);
        assert!((grey[1] - want).abs() < 0.02, "{grey:?} vs {want}");
    }
}
