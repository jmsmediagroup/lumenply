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

use nge_tiles::{Raster, Rgba};

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
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
    let img = image::open(path)?.to_rgba8();
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
        let dir = std::env::temp_dir().join("nge-io-test");
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
        let dir = std::env::temp_dir().join("nge-io-test");
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
