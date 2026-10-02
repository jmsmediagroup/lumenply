//! OpenRaster (`.ora`) import and export, for interchange with GIMP,
//! Krita and MyPaint.
//!
//! An `.ora` is a zip whose first entry is an uncompressed `mimetype`
//! (`image/openraster`), a `stack.xml` describing the layer tree
//! (first child = topmost layer), one sRGB PNG per layer under `data/`,
//! and a merged preview. The format has no masks, adjustments or live
//! filters: masks are baked into the exported alpha, text is exported as
//! pixels, and adjustment/filter layers are skipped, each with a warning.

use std::io::{Cursor, Read, Write};
use std::path::Path;

use lumenply_doc::{BlendMode, Document, Layer, LayerContent, LayerId};
use lumenply_tiles::{Raster, Rect, Rgba, TileStore};
use quick_xml::events::Event;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::psd::Report;
use crate::{linear_to_srgb, srgb_to_linear, IoError};

#[derive(Debug, thiserror::Error)]
pub enum OraError {
    #[error("not an OpenRaster file: {0}")]
    NotOra(String),
    #[error("corrupt OpenRaster file: {0}")]
    Corrupt(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("xml error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
}

impl From<IoError> for OraError {
    fn from(e: IoError) -> Self {
        OraError::Corrupt(e.to_string())
    }
}

/// Largest canvas or layer dimension accepted from a file.
const MAX_DIM: u32 = 100_000;
/// Largest decompressed entry (a 16k × 16k RGBA PNG).
const MAX_ENTRY: u64 = 1 << 30;

fn blend_to_ora(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "svg:src-over",
        BlendMode::Multiply => "svg:multiply",
        BlendMode::Screen => "svg:screen",
        BlendMode::Overlay => "svg:overlay",
        BlendMode::Darken => "svg:darken",
        BlendMode::Lighten => "svg:lighten",
        BlendMode::Difference => "svg:difference",
        BlendMode::Add => "svg:plus",
        BlendMode::HardLight => "svg:hard-light",
        BlendMode::SoftLight => "svg:soft-light",
    }
}

fn blend_from_ora(op: &str) -> Option<BlendMode> {
    Some(match op {
        "svg:src-over" => BlendMode::Normal,
        "svg:multiply" => BlendMode::Multiply,
        "svg:screen" => BlendMode::Screen,
        "svg:overlay" => BlendMode::Overlay,
        "svg:darken" => BlendMode::Darken,
        "svg:lighten" => BlendMode::Lighten,
        "svg:difference" => BlendMode::Difference,
        "svg:plus" | "svg:add" => BlendMode::Add,
        "svg:hard-light" => BlendMode::HardLight,
        "svg:soft-light" => BlendMode::SoftLight,
        _ => return None,
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---- export -------------------------------------------------------------------------------

/// Write the document as `.ora`.
pub fn save(path: impl AsRef<Path>, doc: &Document) -> Result<Report<()>, OraError> {
    let mut warnings = Vec::new();
    doc.for_each_layer(|l| {
        if !l.effects.is_empty() {
            warnings.push(format!(
                "layer '{}': layer effects are not part of OpenRaster and were dropped",
                l.name
            ));
        }
        // Without effects, fill and opacity fade the same pixels, so fill
        // folds into the written opacity.
        if l.fill_opacity < 1.0 {
            warnings.push(format!(
                "layer '{}': fill opacity was folded into the layer opacity",
                l.name
            ));
        }
    });
    let file = std::fs::File::create(path)?;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(file));
    let stored: FileOptions = FileOptions::default().compression_method(CompressionMethod::Stored);
    let deflate: FileOptions = FileOptions::default().compression_method(CompressionMethod::Deflated);

    // The mimetype must be the first entry and uncompressed.
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"image/openraster")?;

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<image version=\"0.0.3\" w=\"{}\" h=\"{}\" xres=\"72\" yres=\"72\">\n<stack>\n",
        doc.width, doc.height
    ));
    let mut counter = 0usize;
    for l in doc.layers().iter().rev() {
        write_layer(&mut zip, deflate, l, &mut xml, &mut counter, &mut warnings, 1)?;
    }
    xml.push_str("</stack>\n</image>\n");
    zip.start_file("stack.xml", deflate)?;
    zip.write_all(xml.as_bytes())?;

    // Merged preview and thumbnail, required of writers by the spec.
    let merged = lumenply_render::composite_raster(doc);
    zip.start_file("mergedimage.png", stored)?;
    zip.write_all(&encode_png(&merged)?)?;
    let thumb = scale_to_fit(&merged, 256);
    zip.start_file("Thumbnails/thumbnail.png", stored)?;
    zip.write_all(&encode_png(&thumb)?)?;

    let mut w = zip.finish()?;
    w.flush()?;
    Ok(Report { value: (), warnings })
}

fn write_layer<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    opts: FileOptions,
    layer: &Layer,
    xml: &mut String,
    counter: &mut usize,
    warnings: &mut Vec<String>,
    depth: usize,
) -> Result<(), OraError> {
    let indent = "  ".repeat(depth);
    let vis = if layer.visible { "visible" } else { "hidden" };
    match &layer.content {
        LayerContent::Group(children) => {
            xml.push_str(&format!(
                "{indent}<stack name=\"{}\" visibility=\"{vis}\" opacity=\"{:.4}\" \
                 composite-op=\"{}\" isolation=\"{}\">\n",
                xml_escape(&layer.name),
                layer.opacity * layer.fill_opacity,
                blend_to_ora(layer.blend),
                if layer.pass_through { "auto" } else { "isolate" },
            ));
            if layer.mask.is_some() {
                warnings.push(format!(
                    "group '{}': OpenRaster has no group masks; the mask was dropped",
                    layer.name
                ));
            }
            for c in children.iter().rev() {
                write_layer(zip, opts, c, xml, counter, warnings, depth + 1)?;
            }
            xml.push_str(&format!("{indent}</stack>\n"));
        }
        LayerContent::Adjustment(_) | LayerContent::Filter(_) => {
            warnings.push(format!(
                "layer '{}': OpenRaster cannot express adjustment or live filter layers; skipped",
                layer.name
            ));
        }
        LayerContent::Pixel(_)
        | LayerContent::Text(_)
        | LayerContent::Smart(_)
        | LayerContent::Fill(_)
        | LayerContent::Shape(_) => {
            if let LayerContent::Shape(sh) = &layer.content {
                warnings.push(format!(
                    "shape layer '{}' ({}) was exported as pixels",
                    layer.name,
                    sh.geometry.name()
                ));
            }
            if matches!(layer.content, LayerContent::Text(_)) {
                warnings.push(format!("text layer '{}' was exported as pixels", layer.name));
            }
            if let LayerContent::Fill(f) = &layer.content {
                warnings.push(format!(
                    "fill layer '{}' ({}) was exported as pixels",
                    layer.name,
                    f.fill.name()
                ));
            }
            if matches!(layer.content, LayerContent::Smart(_)) {
                warnings.push(format!("smart object '{}' was exported as pixels", layer.name));
            }
            if layer.smart_filters.is_active() {
                warnings.push(format!(
                    "layer '{}': smart filters were baked into its pixels",
                    layer.name
                ));
            }
            // raster_store() is the filtered pixels when there are any.
            let Some(store) = layer.raster_store() else {
                return Ok(());
            };
            let bounds = store.content_bounds().unwrap_or(Rect::new(0, 0, 1, 1));
            let mut raster = store.to_raster(bounds);
            if let Some(m) = layer.mask.as_ref().filter(|m| m.enabled) {
                warnings.push(format!(
                    "layer '{}': the mask was baked into the exported alpha",
                    layer.name
                ));
                for y in 0..bounds.h as i32 {
                    for x in 0..bounds.w as i32 {
                        let v = m.value(bounds.x + x, bounds.y + y);
                        if v < 1.0 {
                            let p = raster.get(x as u32, y as u32);
                            raster.set(x as u32, y as u32, p.scale(v));
                        }
                    }
                }
            }
            let src = format!("data/layer{counter}.png");
            *counter += 1;
            zip.start_file(&src, opts)?;
            zip.write_all(&encode_png(&raster)?)?;
            xml.push_str(&format!(
                "{indent}<layer name=\"{}\" src=\"{src}\" x=\"{}\" y=\"{}\" \
                 opacity=\"{:.4}\" visibility=\"{vis}\" composite-op=\"{}\"/>\n",
                xml_escape(&layer.name),
                bounds.x,
                bounds.y,
                layer.opacity * layer.fill_opacity,
                blend_to_ora(layer.blend),
            ));
        }
    }
    Ok(())
}

fn encode_png(raster: &Raster) -> Result<Vec<u8>, OraError> {
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
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn scale_to_fit(src: &Raster, max: u32) -> Raster {
    let scale = (max as f32 / src.width.max(src.height).max(1) as f32).min(1.0);
    let (w, h) = (
        ((src.width as f32 * scale) as u32).max(1),
        ((src.height as f32 * scale) as u32).max(1),
    );
    let mut out = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let sx = ((x as f32 + 0.5) / scale) as u32;
            let sy = ((y as f32 + 0.5) / scale) as u32;
            out.set(x, y, src.get(sx.min(src.width - 1), sy.min(src.height - 1)));
        }
    }
    out
}

// ---- import -------------------------------------------------------------------------------

/// Read an `.ora` into a document.
pub fn load(path: impl AsRef<Path>) -> Result<Report<Document>, OraError> {
    let file = std::fs::File::open(path)?;
    let mut zip = ZipArchive::new(std::io::BufReader::new(file))?;
    let mut warnings = Vec::new();

    match read_entry(&mut zip, "mimetype") {
        Ok(m) if m == b"image/openraster" => {}
        Ok(_) => return Err(OraError::NotOra("wrong mimetype".into())),
        Err(_) => warnings.push("missing mimetype entry; continuing anyway".into()),
    }
    let xml = read_entry(&mut zip, "stack.xml").map_err(|_| OraError::NotOra("missing stack.xml".into()))?;
    let xml = String::from_utf8_lossy(&xml).into_owned();

    let mut reader = quick_xml::Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let (mut width, mut height) = (0u32, 0u32);
    // Stack of layer lists being built; layers arrive top-first, so each
    // finished list is reversed into bottom-to-top order.
    let mut stacks: Vec<Vec<Layer>> = Vec::new();
    // Group shells awaiting their children, parallel to `stacks` minus the root.
    let mut groups: Vec<Layer> = Vec::new();
    let mut next_id: LayerId = 1;
    let mut alloc = || {
        let id = next_id;
        next_id += 1;
        id
    };

    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name();
                let tag = name.as_ref();
                let attrs = |key: &str| -> Option<String> {
                    e.attributes().flatten().find_map(|a| {
                        (a.key.as_ref() == key.as_bytes()).then(|| {
                            a.unescape_value()
                                .map(|v| v.into_owned())
                                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned())
                        })
                    })
                };
                match tag {
                    b"image" => {
                        width = attrs("w").and_then(|v| v.parse().ok()).unwrap_or(0);
                        height = attrs("h").and_then(|v| v.parse().ok()).unwrap_or(0);
                        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
                            return Err(OraError::Corrupt(format!(
                                "canvas {width}×{height} is not a sane size"
                            )));
                        }
                    }
                    b"stack" => {
                        if stacks.is_empty() {
                            stacks.push(Vec::new()); // root
                        } else {
                            let id = alloc();
                            let mut g = Layer::group(id, attrs("name").unwrap_or_else(|| "Group".into()));
                            apply_common(&mut g, &attrs, &mut warnings);
                            // "auto" means non-isolated; our pass-through.
                            g.pass_through = attrs("isolation").as_deref() == Some("auto");
                            groups.push(g);
                            stacks.push(Vec::new());
                        }
                    }
                    b"layer" => {
                        let src = attrs("src").unwrap_or_default();
                        let x: i32 = attrs("x").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let y: i32 = attrs("y").and_then(|v| v.parse().ok()).unwrap_or(0);
                        if x.abs() > MAX_DIM as i32 || y.abs() > MAX_DIM as i32 {
                            return Err(OraError::Corrupt(format!("layer offset {x},{y} out of range")));
                        }
                        let id = alloc();
                        let mut l = Layer::pixel(id, attrs("name").unwrap_or_else(|| "Layer".into()));
                        apply_common(&mut l, &attrs, &mut warnings);
                        match decode_layer_png(&mut zip, &src) {
                            Ok(raster) => {
                                *l.pixels_mut().expect("pixel layer") = TileStore::from_raster(&raster, x, y);
                            }
                            Err(e) => warnings.push(format!("layer '{}': {e}", l.name)),
                        }
                        if let Some(list) = stacks.last_mut() {
                            list.push(l);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) if e.name().as_ref() == b"stack" && stacks.len() > 1 => {
                let mut children = stacks.pop().expect("non-empty");
                children.reverse(); // top-first in the file → bottom-to-top
                let mut g = groups.pop().expect("matching group");
                *g.children_mut().expect("group") = children;
                if let Some(list) = stacks.last_mut() {
                    list.push(g);
                }
            }
            _ => {}
        }
    }
    let mut top = stacks.pop().unwrap_or_default();
    top.reverse();
    if width == 0 {
        return Err(OraError::NotOra("no <image> element".into()));
    }
    let doc = Document::from_parts(width, height, top, next_id);
    Ok(Report { value: doc, warnings })
}

fn apply_common(l: &mut Layer, attrs: &impl Fn(&str) -> Option<String>, warnings: &mut Vec<String>) {
    if let Some(v) = attrs("opacity").and_then(|v| v.parse::<f32>().ok()) {
        l.opacity = v.clamp(0.0, 1.0);
    }
    if let Some(v) = attrs("visibility") {
        l.visible = v != "hidden";
    }
    if let Some(op) = attrs("composite-op") {
        match blend_from_ora(&op) {
            Some(m) => l.blend = m,
            None => warnings.push(format!(
                "layer '{}': unsupported composite-op '{op}', using normal",
                l.name
            )),
        }
    }
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>, OraError> {
    let f = zip
        .by_name(name)
        .map_err(|_| OraError::Corrupt(format!("missing entry {name}")))?;
    let mut buf = Vec::new();
    f.take(MAX_ENTRY + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_ENTRY {
        return Err(OraError::Corrupt(format!("entry {name} is implausibly large")));
    }
    Ok(buf)
}

fn decode_layer_png<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, src: &str) -> Result<Raster, OraError> {
    if src.contains("..") {
        return Err(OraError::Corrupt(format!("suspicious src path {src}")));
    }
    let bytes = read_entry(zip, src)?;
    let reader = image::io::Reader::new(Cursor::new(&bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return Err(OraError::Corrupt(format!(
            "layer image {w}×{h} is not a sane size"
        )));
    }
    let img = image::load_from_memory(&bytes)?.to_rgba8();
    let mut out = Raster::new(w, h);
    for (i, px) in img.pixels().enumerate() {
        out.pixels[i] = Rgba::from_straight(
            srgb_to_linear(px[0]),
            srgb_to_linear(px[1]),
            srgb_to_linear(px[2]),
            px[3] as f32 / 255.0,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::{Adjustment, Mask};

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("nge-ora-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn fill_opacity_folds_into_the_written_opacity() {
        let mut doc = Document::new(8, 8);
        let id = doc.add_pixel_layer("Faded");
        {
            let l = doc.layer_mut(id).unwrap();
            l.pixels_mut()
                .unwrap()
                .set_pixel(1, 1, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
            l.opacity = 0.5;
            l.fill_opacity = 0.4;
        }
        let path = temp("fill.ora");
        let rep = save(&path, &doc).unwrap();
        assert_eq!(rep.warnings.len(), 1, "{:?}", rep.warnings);
        assert!(rep.warnings[0].contains("fill opacity"));
        let back = load(&path).unwrap().value;
        // 50% opacity × 40% fill = 20%, at full fill.
        assert!((back.layers()[0].opacity - 0.2).abs() < 1e-3);
        assert_eq!(back.layers()[0].fill_opacity, 1.0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ora_round_trip_keeps_tree_pixels_and_properties() {
        let mut doc = Document::new(300, 200);
        let bg = doc.add_pixel_layer("Background");
        let fill = Raster::filled(300, 200, Rgba::from_straight(0.9, 0.8, 0.7, 1.0));
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);

        let g = doc.add_group("Group ü <&>");
        let inner_id = doc.alloc_id();
        let mut inner = Layer::pixel(inner_id, "inner");
        inner
            .pixels_mut()
            .unwrap()
            .set_pixel(150, 100, Rgba::from_straight(0.2, 0.4, 0.6, 1.0));
        inner.opacity = 0.5;
        inner.blend = BlendMode::Multiply;
        doc.layer_mut(g).unwrap().children_mut().unwrap().push(inner);
        doc.layer_mut(g).unwrap().visible = false;
        doc.layer_mut(g).unwrap().pass_through = true;

        let top = doc.add_pixel_layer("Offset");
        doc.layer_mut(top).unwrap().pixels_mut().unwrap().set_pixel(
            20,
            30,
            Rgba::from_straight(1.0, 0.0, 0.0, 0.5),
        );

        let path = temp("rt.ora");
        save(&path, &doc).unwrap();
        let back = load(&path).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let back = back.value;

        assert_eq!((back.width, back.height), (300, 200));
        let layers = back.layers();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].name, "Background");
        assert_eq!(layers[1].name, "Group ü <&>");
        assert!(!layers[1].visible);
        assert!(layers[1].pass_through, "isolation=auto round-trips");
        let inner = &layers[1].children().unwrap()[0];
        assert!((inner.opacity - 0.5).abs() < 1e-3);
        assert_eq!(inner.blend, BlendMode::Multiply);
        let p = inner.pixels().unwrap().get_pixel(150, 100).to_straight();
        assert!((p[0] - 0.2).abs() < 0.01 && (p[2] - 0.6).abs() < 0.01, "{p:?}");
        // The offset layer keeps its position.
        let p = layers[2].pixels().unwrap().get_pixel(20, 30).to_straight();
        assert!((p[0] - 1.0).abs() < 0.01 && (p[3] - 0.5).abs() < 0.01, "{p:?}");

        // The zip starts with the uncompressed mimetype, as the spec demands.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert!(
            bytes[..100].windows(16).any(|w| w == b"image/openraster"),
            "mimetype near the start"
        );
    }

    #[test]
    fn unsupported_layers_warn_and_masks_bake() {
        let mut doc = Document::new(64, 64);
        let bg = doc.add_pixel_layer("bg");
        let fill = Raster::filled(64, 64, Rgba::WHITE);
        *doc.layer_mut(bg).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&fill, 0, 0);
        let mut mask = Mask::reveal_all();
        mask.fill_rect(Rect::new(0, 0, 32, 64), 0.0);
        doc.layer_mut(bg).unwrap().mask = Some(mask);
        doc.add_adjustment(Adjustment::Invert);

        let path = temp("warn.ora");
        let rep = save(&path, &doc).unwrap();
        assert_eq!(rep.warnings.len(), 2, "{:?}", rep.warnings);

        let back = load(&path).unwrap().value;
        assert_eq!(back.layer_count(), 1, "adjustment skipped");
        let px = back.layers()[0].pixels().unwrap();
        assert!(px.get_pixel(10, 10).a < 0.01, "masked-out side is transparent");
        assert!(px.get_pixel(50, 10).a > 0.99, "revealed side kept");
    }

    #[test]
    fn malformed_oras_error_instead_of_panicking() {
        let path = temp("bogus.ora");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(load(&path).is_err());

        // A zip without stack.xml.
        let path = temp("nostack.ora");
        let mut zip = ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()));
        let stored: FileOptions = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"image/openraster").unwrap();
        zip.finish().unwrap();
        assert!(load(&path).is_err());

        // An absurd canvas size.
        let path = temp("huge.ora");
        let mut zip = ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()));
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"image/openraster").unwrap();
        zip.start_file("stack.xml", stored).unwrap();
        zip.write_all(b"<image w=\"4000000000\" h=\"5\"><stack></stack></image>")
            .unwrap();
        zip.finish().unwrap();
        assert!(load(&path).is_err());
    }
}
