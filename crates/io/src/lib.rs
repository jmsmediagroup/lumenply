//! Image codecs and colour conversion.
//!
//! Files on disk are 8-bit sRGB with straight alpha; the engine works in
//! linear light with premultiplied alpha, so every load and save converts.
//! Only PNG and JPEG are wired up today; TIFF, WebP, JPEG XL, HEIF, RAW,
//! EXR and PSD each get their own module here as the roadmap reaches them.
//! The native project format lives in [`project`].

pub mod ora;
pub mod project;
pub mod psd;

use std::path::Path;

use lumenply_tiles::{Raster, Rgba};

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec error: {0}")]
    Codec(String),
}

/// sRGB transfer function, 8-bit in, linear `[0, 1]` out.
#[inline]
pub fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer function, linear `[0, 1]` in, 8-bit out.
#[inline]
pub fn linear_to_srgb(v: f32) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

/// Load a PNG or JPEG as a linear, premultiplied raster.
pub fn load(path: impl AsRef<Path>) -> Result<Raster, IoError> {
    let path = path.as_ref();
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exr")) {
        return load_exr(path);
    }
    let img = image::open(path)?;
    // Keep the full precision of 16-bit sources (PNG, TIFF) instead of
    // truncating them to 8 bits on the way in.
    if img.color().bits_per_pixel() > 32 {
        let img = img.to_rgba16();
        let (w, h) = img.dimensions();
        let mut out = Raster::new(w, h);
        for (i, px) in img.pixels().enumerate() {
            let c = |v: u16| srgb_to_linear_f(v as f32 / 65535.0);
            out.pixels[i] = Rgba::from_straight(c(px[0]), c(px[1]), c(px[2]), px[3] as f32 / 65535.0);
        }
        return Ok(out);
    }
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    let mut lut = [0f32; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = srgb_to_linear(i as u8);
    }
    let mut out = Raster::new(w, h);
    for (i, px) in img.pixels().enumerate() {
        let a = px[3] as f32 / 255.0;
        out.pixels[i] = Rgba::from_straight(lut[px[0] as usize], lut[px[1] as usize], lut[px[2] as usize], a);
    }
    Ok(out)
}

/// The sRGB transfer function on a normalised value (no 8-bit rounding).
pub fn srgb_to_linear_f(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of [`srgb_to_linear_f`].
pub fn linear_to_srgb_f(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Read an OpenEXR image. EXR stores linear-light floats, which is this
/// engine's native space, so pixels come through untouched (alpha is taken
/// as straight and premultiplied on the way in).
pub fn load_exr(path: impl AsRef<Path>) -> Result<Raster, IoError> {
    use exr::prelude::*;
    let img = read_first_rgba_layer_from_file(
        path,
        |resolution: Vec2<usize>, _| Raster::new(resolution.width() as u32, resolution.height() as u32),
        |raster: &mut Raster, pos: Vec2<usize>, (r, g, b, a): (f32, f32, f32, f32)| {
            let a = a.clamp(0.0, 1.0);
            raster.set(
                pos.x() as u32,
                pos.y() as u32,
                Rgba::new(r.max(0.0) * a, g.max(0.0) * a, b.max(0.0) * a, a),
            );
        },
    )
    .map_err(|e| IoError::Codec(e.to_string()))?;
    Ok(img.layer_data.channel_data.pixels)
}

/// Save a raster as OpenEXR (linear-light f32, straight alpha) — the
/// lossless interchange for this engine's native pixel format.
pub fn save_exr(path: impl AsRef<Path>, raster: &Raster) -> Result<(), IoError> {
    use exr::prelude::*;
    write_rgba_file(path, raster.width as usize, raster.height as usize, |x, y| {
        let p = raster.get(x as u32, y as u32);
        let [r, g, b, a] = p.to_straight();
        (r, g, b, a)
    })
    .map_err(|e| IoError::Codec(e.to_string()))?;
    Ok(())
}

/// Save a raster as a 16-bit sRGB PNG or TIFF (by extension) with straight
/// alpha, keeping precision an 8-bit export would round away.
pub fn save_16bit(path: impl AsRef<Path>, raster: &Raster) -> Result<(), IoError> {
    let path = path.as_ref();
    let mut buf: Vec<u16> = Vec::with_capacity(raster.pixels.len() * 4);
    for p in &raster.pixels {
        let [r, g, b, a] = p.to_straight();
        let q = |v: f32| (linear_to_srgb_f(v) * 65535.0 + 0.5) as u16;
        buf.extend_from_slice(&[q(r), q(g), q(b), (a.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16]);
    }
    let img = image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(raster.width, raster.height, buf)
        .expect("buffer size matches dimensions");
    let format = match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff") => {
            image::ImageFormat::Tiff
        }
        _ => image::ImageFormat::Png,
    };
    image::DynamicImage::ImageRgba16(img).save_with_format(path, format)?;
    Ok(())
}

/// Save a raster as an 8-bit sRGB PNG with straight alpha.
pub fn save_png(path: impl AsRef<Path>, raster: &Raster) -> Result<(), IoError> {
    let mut buf = Vec::with_capacity(raster.pixels.len() * 4);
    for p in &raster.pixels {
        let [r, g, b, a] = p.to_straight();
        buf.extend_from_slice(&[
            linear_to_srgb(r),
            linear_to_srgb(g),
            linear_to_srgb(b),
            (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        ]);
    }
    let img =
        image::RgbaImage::from_raw(raster.width, raster.height, buf).expect("buffer size matches dimensions");
    img.save_with_format(path, image::ImageFormat::Png)?;
    Ok(())
}

/// Save a raster as JPEG (no alpha: composited over white), quality 1–100.
pub fn save_jpeg(path: impl AsRef<Path>, raster: &Raster, quality: u8) -> Result<(), IoError> {
    let mut buf = Vec::with_capacity(raster.pixels.len() * 3);
    for p in &raster.pixels {
        let over_white = p.over(Rgba::WHITE);
        let [r, g, b, _] = over_white.to_straight();
        buf.extend_from_slice(&[linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b)]);
    }
    let img =
        image::RgbImage::from_raw(raster.width, raster.height, buf).expect("buffer size matches dimensions");
    let file = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::new(file);
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, quality.clamp(1, 100));
    img.write_with_encoder(enc)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_round_trip_is_close_and_opaque() {
        let mut r = Raster::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                r.set(
                    x,
                    y,
                    Rgba::from_straight(0.2, 0.6, 0.9, if x < 8 { 1.0 } else { 0.0 }),
                );
            }
        }
        let dir = std::env::temp_dir().join("lumenply-io-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.jpg");
        save_jpeg(&path, &r, 95).unwrap();
        let back = load(&path).unwrap();
        let p = back.get(2, 2).to_straight();
        assert!((p[1] - 0.6).abs() < 0.08 && (p[3] - 1.0).abs() < 1e-6, "{p:?}");
        let transparent_side = back.get(12, 2).to_straight();
        assert!(
            transparent_side[0] > 0.9,
            "transparent pixels land on white: {transparent_side:?}"
        );
    }

    #[test]
    fn exr_round_trips_linear_floats_exactly_enough() {
        let dir = std::env::temp_dir().join("nge-io16-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = Raster::new(3, 1);
        // Values an 8- or 16-bit sRGB file could not keep: tiny, >8-bit
        // precision, and HDR-ish handled by clamping alpha only.
        r.set(0, 0, Rgba::from_straight(0.001234, 0.5, 0.25, 1.0));
        r.set(1, 0, Rgba::from_straight(0.333333, 0.123456, 0.9, 0.5));
        r.set(2, 0, Rgba::TRANSPARENT);
        let path = dir.join("rt.exr");
        save_exr(&path, &r).unwrap();
        let back = load_exr(&path).unwrap();
        for i in 0..3u32 {
            let (a, b) = (r.get(i, 0), back.get(i, 0));
            assert!(
                (a.r - b.r).abs() < 1e-6
                    && (a.g - b.g).abs() < 1e-6
                    && (a.b - b.b).abs() < 1e-6
                    && (a.a - b.a).abs() < 1e-6,
                "pixel {i}: {a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn sixteen_bit_round_trip_keeps_sub_8bit_precision() {
        let dir = std::env::temp_dir().join("nge-io16-test");
        std::fs::create_dir_all(&dir).unwrap();
        // Two values that quantise to the same 8-bit byte but different
        // 16-bit values.
        let a = 0.5000f32;
        let b = 0.5020f32;
        let mut r = Raster::new(2, 1);
        r.set(0, 0, Rgba::from_straight(a, a, a, 1.0));
        r.set(1, 0, Rgba::from_straight(b, b, b, 0.7));
        for name in ["deep.png", "deep.tif"] {
            let path = dir.join(name);
            save_16bit(&path, &r).unwrap();
            let back = load(&path).unwrap();
            let pa = back.get(0, 0).to_straight();
            let pb = back.get(1, 0).to_straight();
            assert!((pa[0] - a).abs() < 1e-4, "{name}: {} vs {a}", pa[0]);
            assert!((pb[0] - b).abs() < 1e-4, "{name}: {} vs {b}", pb[0]);
            assert!(
                (pb[0] - pa[0]).abs() > 1e-4,
                "{name}: 16-bit values stayed distinct"
            );
            assert!((pb[3] - 0.7).abs() < 1e-4, "{name}: alpha precision");
        }
    }

    #[test]
    fn srgb_round_trips_every_byte() {
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(v)), v);
        }
    }

    #[test]
    fn mid_grey_is_darker_in_linear() {
        let l = srgb_to_linear(128);
        assert!(l > 0.21 && l < 0.22, "{l}");
    }

    #[test]
    fn png_round_trip() {
        let mut r = Raster::new(3, 2);
        r.set(0, 0, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
        r.set(1, 0, Rgba::from_straight(0.0, 1.0, 0.0, 0.5));
        r.set(2, 1, Rgba::WHITE);
        let dir = std::env::temp_dir().join("lumenply-io-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.png");
        save_png(&path, &r).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.width, 3);
        let p = back.get(1, 0).to_straight();
        assert!((p[1] - 1.0).abs() < 0.01 && (p[3] - 0.5).abs() < 0.01, "{p:?}");
        assert_eq!(back.get(0, 1), Rgba::TRANSPARENT);
    }
}
