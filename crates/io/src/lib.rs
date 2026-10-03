//! Image codecs and colour conversion.
//!
//! Files on disk are 8-bit sRGB with straight alpha; the engine works in
//! linear light with premultiplied alpha, so every load and save converts.
//! Only PNG and JPEG are wired up today; TIFF, WebP, JPEG XL, HEIF, RAW,
//! EXR and PSD each get their own module here as the roadmap reaches them.
//! The native project format lives in [`project`].

pub mod abr;
pub mod lut_files;
pub mod ora;
pub mod pattern_files;
pub mod pdf;
pub mod project;
pub mod psd;
mod psd_channels;
mod psd_guides;
pub mod raw;
pub mod resolution;
pub mod system_image;

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
    if raw::is_raw(path) {
        return raw::load_raw(path);
    }
    if system_image::is_system_format(path) {
        return system_image::load_system(path);
    }
    let (img, icc) = decode_with_icc(path)?;
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
    let mut img = img.to_rgba8();
    // Colour management v1: an embedded ICC profile converts to sRGB
    // before linearisation (8-bit path; deeper imports assume sRGB).
    if let Some(icc) = icc {
        apply_icc_to_srgb(&mut img, &icc);
    }
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

/// Decode an image, also fishing out its embedded ICC profile when the
/// container carries one (PNG iCCP, JPEG APP2).
fn decode_with_icc(path: &Path) -> Result<(image::DynamicImage, Option<Vec<u8>>), IoError> {
    use image::ImageDecoder;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    match ext.as_str() {
        "png" => {
            let mut dec = image::codecs::png::PngDecoder::new(reader)?;
            let icc = dec.icc_profile();
            Ok((image::DynamicImage::from_decoder(dec)?, icc))
        }
        "jpg" | "jpeg" => {
            let mut dec = image::codecs::jpeg::JpegDecoder::new(reader)?;
            let icc = dec.icc_profile();
            Ok((image::DynamicImage::from_decoder(dec)?, icc))
        }
        _ => Ok((image::open(path)?, None)),
    }
}

/// Convert straight-alpha RGBA8 pixels from `icc`'s colour space to sRGB
/// in place. An unparseable or non-RGB profile is a no-op — never worse
/// than the old behaviour of ignoring it.
pub fn apply_icc_to_srgb(img: &mut image::RgbaImage, icc: &[u8]) {
    let Some(src) = qcms::Profile::new_from_slice(icc, false) else {
        return;
    };
    let dst = qcms::Profile::new_sRGB();
    let Some(transform) = qcms::Transform::new(&src, &dst, qcms::DataType::RGBA8, qcms::Intent::Perceptual)
    else {
        return;
    };
    transform.apply(img.as_mut());
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

/// A compact public-domain (CC0) sRGB v2 profile, embedded in JPEG exports
/// so colour-managed readers treat the pixels as what they are. From
/// Compact-ICC-Profiles (saucecontrol), 456 bytes.
pub const SRGB_ICC: &[u8] = include_bytes!("../profiles/sRGB-v2-micro.icc");

/// Write a PNG tagged as sRGB (the `sRGB` chunk — the standard way to
/// declare sRGB, honoured by colour-managed readers).
fn write_png_srgb(
    path: &Path,
    width: u32,
    height: u32,
    depth: png::BitDepth,
    data: &[u8],
) -> Result<(), IoError> {
    let file = std::fs::File::create(path)?;
    png_srgb_into(std::io::BufWriter::new(file), width, height, depth, data)
}

fn png_srgb_into(
    out: impl std::io::Write,
    width: u32,
    height: u32,
    depth: png::BitDepth,
    data: &[u8],
) -> Result<(), IoError> {
    let mut enc = png::Encoder::new(out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(depth);
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = enc.write_header().map_err(|e| IoError::Codec(e.to_string()))?;
    writer
        .write_image_data(data)
        .map_err(|e| IoError::Codec(e.to_string()))?;
    Ok(())
}

/// Save a raster as a 16-bit sRGB PNG or TIFF (by extension) with straight
/// alpha, keeping precision an 8-bit export would round away. The PNG is
/// tagged as sRGB; TIFF has no such lightweight tag and stays untagged.
pub fn save_16bit(path: impl AsRef<Path>, raster: &Raster) -> Result<(), IoError> {
    let path = path.as_ref();
    let mut buf: Vec<u16> = Vec::with_capacity(raster.pixels.len() * 4);
    for p in &raster.pixels {
        let [r, g, b, a] = p.to_straight();
        let q = |v: f32| (linear_to_srgb_f(v) * 65535.0 + 0.5) as u16;
        buf.extend_from_slice(&[q(r), q(g), q(b), (a.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16]);
    }
    let is_tiff = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(e) if e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff")
    );
    if is_tiff {
        let img = image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(raster.width, raster.height, buf)
            .expect("buffer size matches dimensions");
        image::DynamicImage::ImageRgba16(img).save_with_format(path, image::ImageFormat::Tiff)?;
        return Ok(());
    }
    let bytes: Vec<u8> = buf.iter().flat_map(|v| v.to_be_bytes()).collect();
    write_png_srgb(path, raster.width, raster.height, png::BitDepth::Sixteen, &bytes)
}

/// 8-bit sRGB bytes with straight alpha; `opaque` composites over white.
fn rgba8(raster: &Raster, opaque: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(raster.pixels.len() * 4);
    for p in &raster.pixels {
        let p = if opaque { p.over(Rgba::WHITE) } else { *p };
        let [r, g, b, a] = p.to_straight();
        buf.extend_from_slice(&[
            linear_to_srgb(r),
            linear_to_srgb(g),
            linear_to_srgb(b),
            (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        ]);
    }
    buf
}

/// Save a raster as an 8-bit sRGB PNG with straight alpha, tagged as sRGB.
pub fn save_png(path: impl AsRef<Path>, raster: &Raster) -> Result<(), IoError> {
    write_png_srgb(
        path.as_ref(),
        raster.width,
        raster.height,
        png::BitDepth::Eight,
        &rgba8(raster, false),
    )
}

/// An 8-bit sRGB PNG (tagged sRGB) in memory; `transparency` false
/// composites over white.
pub fn encode_png(raster: &Raster, transparency: bool) -> Result<Vec<u8>, IoError> {
    let mut out = Vec::new();
    png_srgb_into(
        &mut out,
        raster.width,
        raster.height,
        png::BitDepth::Eight,
        &rgba8(raster, !transparency),
    )?;
    Ok(out)
}

/// A lossless WebP in memory; `transparency` false composites over white.
pub fn encode_webp(raster: &Raster, transparency: bool) -> Result<Vec<u8>, IoError> {
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(
            &rgba8(raster, !transparency),
            raster.width,
            raster.height,
            image_webp::ColorType::Rgba8,
        )
        .map_err(|e| IoError::Codec(e.to_string()))?;
    Ok(out)
}

/// A GIF: 256 colours chosen for the image (NeuQuant), one transparent
/// index for pixels under half opacity when `transparency` is on (GIF has
/// no partial alpha), else on white.
pub fn encode_gif(raster: &Raster, transparency: bool) -> Result<Vec<u8>, IoError> {
    let rgba = rgba8(raster, !transparency);
    let n = (raster.width * raster.height) as usize;
    let clear: Vec<bool> = rgba.chunks_exact(4).map(|p| p[3] < 128).collect();
    let any_clear = clear.iter().any(|&c| c);
    // Quantise the visible pixels only; index 255 is kept for transparency.
    let opaque: Vec<u8> = rgba
        .chunks_exact(4)
        .zip(&clear)
        .filter(|(_, c)| !**c)
        .flat_map(|(p, _)| [p[0], p[1], p[2], 255])
        .collect();
    let colours = if any_clear { 255 } else { 256 };
    // Few colours (graphics, pixel art): an exact palette. Photos: NeuQuant.
    let mut exact: std::collections::HashMap<[u8; 3], u8> = std::collections::HashMap::new();
    for p in opaque.chunks_exact(4) {
        let key = [p[0], p[1], p[2]];
        let next = exact.len();
        if next >= colours {
            exact.clear();
            break;
        }
        exact.entry(key).or_insert(next as u8);
    }
    let mut palette = vec![0u8; 256 * 3];
    let mut indices = Vec::with_capacity(n);
    if !exact.is_empty() || opaque.is_empty() {
        for (key, &i) in &exact {
            palette[i as usize * 3..i as usize * 3 + 3].copy_from_slice(key);
        }
        for (p, &c) in rgba.chunks_exact(4).zip(&clear) {
            indices.push(if c { 255 } else { exact[&[p[0], p[1], p[2]]] });
        }
    } else {
        let nq = color_quant::NeuQuant::new(10, colours, &opaque);
        let map = nq.color_map_rgb();
        palette[..map.len()].copy_from_slice(&map);
        for (p, &c) in rgba.chunks_exact(4).zip(&clear) {
            indices.push(if c {
                255
            } else {
                nq.index_of(&[p[0], p[1], p[2], 255]) as u8
            });
        }
    }
    let (w, h) = (
        u16::try_from(raster.width).map_err(|_| IoError::Codec("GIF is limited to 65535 px".into()))?,
        u16::try_from(raster.height).map_err(|_| IoError::Codec("GIF is limited to 65535 px".into()))?,
    );
    let mut out = Vec::new();
    {
        let mut enc =
            gif::Encoder::new(&mut out, w, h, &palette).map_err(|e| IoError::Codec(e.to_string()))?;
        let frame = gif::Frame {
            width: w,
            height: h,
            buffer: std::borrow::Cow::Owned(indices),
            transparent: any_clear.then_some(255),
            ..Default::default()
        };
        enc.write_frame(&frame)
            .map_err(|e| IoError::Codec(e.to_string()))?;
    }
    Ok(out)
}

/// Splice an ICC profile into a JPEG stream as an APP2 segment, placed
/// after any APP0 (JFIF) marker so the segment order stays conventional.
fn jpeg_with_icc(jpeg: &[u8], icc: &[u8]) -> Vec<u8> {
    // One segment holds up to ~64KB; our profile is a few hundred bytes.
    let payload_len = 2 + 12 + 2 + icc.len(); // length field + header + seq/count
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 || payload_len > 0xFFFF {
        return jpeg.to_vec();
    }
    let mut at = 2;
    // Skip over an APP0 segment if one follows SOI.
    if jpeg.len() >= at + 4 && jpeg[at] == 0xFF && jpeg[at + 1] == 0xE0 {
        let seg = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        at += 2 + seg;
    }
    let mut out = Vec::with_capacity(jpeg.len() + payload_len + 2);
    out.extend_from_slice(&jpeg[..at.min(jpeg.len())]);
    out.extend_from_slice(&[0xFF, 0xE2]);
    out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    out.extend_from_slice(b"ICC_PROFILE\0");
    out.extend_from_slice(&[1, 1]); // chunk 1 of 1
    out.extend_from_slice(icc);
    out.extend_from_slice(&jpeg[at.min(jpeg.len())..]);
    out
}

/// Save a raster as JPEG (no alpha: composited over white), quality 1–100,
/// with an sRGB ICC profile embedded.
pub fn save_jpeg(path: impl AsRef<Path>, raster: &Raster, quality: u8) -> Result<(), IoError> {
    std::fs::write(path, encode_jpeg(raster, quality)?)?;
    Ok(())
}

/// The JPEG [`save_jpeg`] writes, in memory.
pub fn encode_jpeg(raster: &Raster, quality: u8) -> Result<Vec<u8>, IoError> {
    let mut buf = Vec::with_capacity(raster.pixels.len() * 3);
    for p in &raster.pixels {
        let over_white = p.over(Rgba::WHITE);
        let [r, g, b, _] = over_white.to_straight();
        buf.extend_from_slice(&[linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b)]);
    }
    let img =
        image::RgbImage::from_raw(raster.width, raster.height, buf).expect("buffer size matches dimensions");
    let mut encoded = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality.clamp(1, 100));
    img.write_with_encoder(enc)?;
    Ok(jpeg_with_icc(&encoded, SRGB_ICC))
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    fn sample() -> Raster {
        let mut r = Raster::new(9, 6);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let a = if i % 4 == 0 { 0.5 } else { 1.0 };
            *p = Rgba::from_straight(srgb_to_linear((i * 25 % 256) as u8), 0.2, 0.7, a);
        }
        r
    }

    #[test]
    fn png_and_webp_encode_losslessly_in_memory() {
        let r = sample();
        let want = rgba8(&r, false);
        let png = encode_png(&r, true).unwrap();
        let back = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(back.into_raw(), want);
        let webp = encode_webp(&r, true).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        let mut dec = image_webp::WebPDecoder::new(std::io::Cursor::new(&webp)).unwrap();
        assert_eq!(dec.dimensions(), (9, 6));
        let mut buf = vec![0u8; dec.output_buffer_size().unwrap()];
        dec.read_image(&mut buf).unwrap();
        assert_eq!(buf, want);
    }

    #[test]
    fn without_transparency_pixels_sit_on_white() {
        let mut r = Raster::new(1, 1);
        r.pixels[0] = Rgba::from_straight(0.0, 0.0, 0.0, 0.5);
        let png = encode_png(&r, false).unwrap();
        let px = image::load_from_memory(&png).unwrap().to_rgba8().into_raw();
        // Half black over white in linear light is linear 0.5 = sRGB 188.
        assert_eq!(px, vec![188, 188, 188, 255]);
    }

    #[test]
    fn jpeg_in_memory_carries_its_srgb_profile() {
        let jpg = encode_jpeg(&sample(), 90).unwrap();
        assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
        assert!(jpg.windows(12).any(|w| w == b"ICC_PROFILE\0"));
        let lo = encode_jpeg(&sample(), 10).unwrap();
        assert!(lo.len() < jpg.len());
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn gif_export_keeps_its_few_colours_and_hard_transparency() {
        let mut r = Raster::new(4, 2);
        r.pixels[0] = Rgba::from_straight(1.0, 0.0, 0.0, 1.0);
        r.pixels[1] = Rgba::from_straight(0.0, 0.0, 1.0, 1.0);
        r.pixels[2] = Rgba::TRANSPARENT;
        r.pixels[3] = Rgba::from_straight(0.0, 1.0, 0.0, 0.3);
        for p in &mut r.pixels[4..] {
            *p = Rgba::from_straight(1.0, 1.0, 1.0, 1.0);
        }
        let bytes = encode_gif(&r, true).unwrap();
        assert_eq!(&bytes[..3], b"GIF");
        let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
        let px = |x, y| back.get_pixel(x, y).0;
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        assert_eq!(px(1, 0), [0, 0, 255, 255]);
        assert_eq!(px(2, 0)[3], 0, "transparent stays transparent");
        assert_eq!(px(3, 0)[3], 0, "under half opacity becomes transparent");
        assert_eq!(px(0, 1), [255, 255, 255, 255]);
    }

    #[test]
    fn bmp_tga_and_gif_files_open() {
        let dir = std::env::temp_dir().join(format!("lumenply-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbaImage::from_fn(3, 2, |x, _| {
            image::Rgba([if x == 0 { 255 } else { 0 }, 128, 0, 255])
        });
        for ext in ["bmp", "tga", "gif"] {
            let path = dir.join(format!("t.{ext}"));
            image::DynamicImage::ImageRgba8(img.clone()).save(&path).unwrap();
            let r = load(&path).unwrap();
            assert_eq!((r.width, r.height), (3, 2), "{ext}");
            let p = r.get(0, 1).to_straight();
            assert!(
                (p[0] - 1.0).abs() < 0.01 && (p[1] - srgb_to_linear(128)).abs() < 0.02,
                "{ext}: {p:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    fn exports_are_tagged_as_srgb() {
        let mut r = Raster::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                r.set(x, y, Rgba::from_straight(0.3, 0.5, 0.7, 1.0));
            }
        }
        let dir = std::env::temp_dir().join("lumenply-io-test");
        std::fs::create_dir_all(&dir).unwrap();

        // PNG (8- and 16-bit) carries the standard sRGB chunk.
        let chunks = |bytes: &[u8]| -> Vec<String> {
            let mut names = Vec::new();
            let mut at = 8; // signature
            while at + 8 <= bytes.len() {
                let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
                names.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
                at += 12 + len; // length + name + data + crc
            }
            names
        };
        for (name, deep) in [("tag8.png", false), ("tag16.png", true)] {
            let path = dir.join(name);
            if deep {
                save_16bit(&path, &r).unwrap();
            } else {
                save_png(&path, &r).unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            let names = chunks(&bytes);
            assert!(names.iter().any(|n| n == "sRGB"), "{name} chunks: {names:?}");
            // The tagged file still loads with the expected values.
            let back = load(&path).unwrap();
            let p = back.get(3, 3).to_straight();
            assert!((p[1] - 0.5).abs() < 0.01, "{name} round trip: {p:?}");
        }

        // JPEG embeds the bundled sRGB profile in an APP2 segment, and the
        // profile itself must be one qcms accepts.
        assert!(
            qcms::Profile::new_from_slice(SRGB_ICC, false).is_some(),
            "bundled profile parses"
        );
        let path = dir.join("tag.jpg");
        save_jpeg(&path, &r, 95).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let pos = bytes
            .windows(12)
            .position(|w| w == b"ICC_PROFILE\0")
            .expect("APP2 ICC marker present");
        assert_eq!(
            &bytes[pos + 14..pos + 14 + 4],
            &SRGB_ICC[..4],
            "profile bytes follow"
        );
        // Our own importer reads the profile back and lands on the same
        // colours (an sRGB-to-sRGB transform is a near-no-op).
        let back = load(&path).unwrap();
        let p = back.get(3, 3).to_straight();
        assert!((p[0] - 0.3).abs() < 0.05 && (p[1] - 0.5).abs() < 0.05, "{p:?}");
    }

    #[test]
    fn icc_conversion_is_safe_and_actually_transforms() {
        // Garbage profiles must be a harmless no-op.
        let mut img = image::RgbaImage::from_pixel(2, 2, image::Rgba([120, 200, 40, 255]));
        let before = img.clone();
        apply_icc_to_srgb(&mut img, b"not an icc profile at all");
        assert_eq!(img, before, "bad profile leaves pixels untouched");

        // With a real wide-gamut profile (the system's Display P3, when
        // present), neutrals stay neutral and saturated colours move.
        let p3 = std::path::Path::new("/System/Library/ColorSync/Profiles/Display P3.icc");
        let Ok(icc) = std::fs::read(p3) else {
            eprintln!("no Display P3 profile on this system; skipping transform check");
            return;
        };
        let mut img = image::RgbaImage::new(3, 1);
        img.put_pixel(0, 0, image::Rgba([128, 128, 128, 255])); // neutral
        img.put_pixel(1, 0, image::Rgba([255, 255, 255, 255])); // white
        img.put_pixel(2, 0, image::Rgba([30, 220, 60, 200])); // P3-ish green
        let before = img.clone();
        apply_icc_to_srgb(&mut img, &icc);
        let g = img.get_pixel(0, 0).0;
        assert!(
            g[0].abs_diff(g[1]) <= 2 && g[1].abs_diff(g[2]) <= 2,
            "grey stays neutral: {g:?}"
        );
        let w = img.get_pixel(1, 0).0;
        assert!(
            w[0] >= 253 && w[1] >= 253 && w[2] >= 253,
            "white stays white: {w:?}"
        );
        assert_ne!(
            img.get_pixel(2, 0),
            before.get_pixel(2, 0),
            "saturated P3 colour is remapped"
        );
        assert_eq!(img.get_pixel(2, 0).0[3], 200, "alpha untouched");
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
