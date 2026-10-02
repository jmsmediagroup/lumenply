//! The demo document: a real photograph (Aoraki at sunrise, CC0 — see
//! `assets/NOTICE.md`) under a small stack of the kind of non-destructive
//! edits a photographer would actually make — a shadow-lifting curve, a
//! masked colour boost on the sky, a warm grade, a soft vignette and a
//! title. Built in the app crate so `lumenply_core::demo` (used by the CLI
//! and engine tests) stays a pure, asset-free synthetic document.

use super::*;
use lumenply_doc::{LayerEffects, Mask, ShadowFx};
use lumenply_tiles::Rgba;

/// The photo, downscaled to 1800 × 1205 (see `assets/NOTICE.md`).
pub(crate) const PHOTO_JPG: &[u8] = include_bytes!("../assets/demo-photo.jpg");

/// Tab label for the demo, which has no file path until it is saved.
pub(crate) const TITLE: &str = "Aoraki demo";

/// Decode the bundled photo into a linear, premultiplied raster (the same
/// conversion `lumenply_io::load` applies to an sRGB JPEG on disk).
pub(crate) fn photo_raster() -> Result<Raster, String> {
    let img = image::load_from_memory_with_format(PHOTO_JPG, image::ImageFormat::Jpeg)
        .map_err(|e| format!("demo photo: {e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    let mut lut = [0f32; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = lumenply_io::srgb_to_linear(i as u8);
    }
    let mut out = Raster::new(w, h);
    for (dst, px) in out.pixels.iter_mut().zip(img.pixels()) {
        *dst = Rgba::from_straight(lut[px[0] as usize], lut[px[1] as usize], lut[px[2] as usize], 1.0);
    }
    Ok(out)
}

/// Build the demo. It opens like a saved project: the edits are already
/// in the layer stack, and the history starts clean.
pub(crate) fn build() -> Result<Editor, String> {
    let photo = photo_raster()?;
    let (w, h) = (photo.width, photo.height);
    let mut ed = Editor::new(Document::new(w, h));
    let err = |e: lumenply_core::EditError| e.to_string();

    ed.execute(&AddPixelLayer::from_raster("Background", photo, 0, 0))
        .map_err(err)?;

    // Open up the dark valley without touching the sky: a gentle lift in
    // the shadows, highlights left where they are.
    add_adjustment(
        &mut ed,
        "Lift shadows",
        Adjustment::Curves {
            points: vec![[0.0, 0.03], [0.22, 0.29], [0.55, 0.6], [1.0, 1.0]],
        },
    )?;

    // A richer sunrise, painted onto the sky only through a soft mask.
    let sky = add_adjustment(
        &mut ed,
        "Sky colour",
        Adjustment::HueSaturation {
            hue: 6.0,
            saturation: 0.32,
            lightness: 0.0,
        },
    )?;
    ed.execute(&SetMask {
        layer: sky,
        mask: Some(sky_mask(w, h)),
    })
    .map_err(err)?;

    // Warm the whole frame a touch: golden highlights, cooler shadows.
    add_adjustment(
        &mut ed,
        "Warm grade",
        Adjustment::ColorBalance {
            shadows: [-0.06, 0.0, 0.1],
            midtones: [0.08, 0.0, -0.06],
            highlights: [0.12, 0.02, -0.12],
            preserve_luminosity: true,
        },
    )?;

    let mut vignette = AddPixelLayer::from_raster("Vignette", vignette(w, h), 0, 0);
    vignette.opacity = 0.6;
    ed.execute(&vignette).map_err(err)?;

    // The title: a small kicker line over a wide-tracked name, both
    // centred in the sky with a soft shadow for legibility.
    let white = [1.0, 1.0, 1.0, 0.94];
    let cx = w as f32 / 2.0;
    let mut kicker = TextLayer::new("MOUNT COOK NATIONAL PARK", cx, h as f32 * 0.17, 27.0, white);
    kicker.align = TextAlign::Center;
    kicker.tracking = 450.0;
    let mut name = TextLayer::new("AORAKI", cx, h as f32 * 0.275, 124.0, white);
    name.align = TextAlign::Center;
    name.bold = true;
    name.tracking = 420.0;
    let mut text_ids = Vec::new();
    for t in [kicker, name] {
        ed.execute(&AddTextLayer { text: t, above: None }).map_err(err)?;
        let id = top_id(&ed)?;
        ed.execute(&SetLayerEffects {
            layer: id,
            effects: LayerEffects {
                drop_shadow: Some(ShadowFx {
                    dx: 0.0,
                    dy: 3.0,
                    blur: 14.0,
                    color: [0.02, 0.01, 0.03],
                    opacity: 0.45,
                }),
                ..LayerEffects::default()
            },
        })
        .map_err(err)?;
        text_ids.push(id);
    }
    ed.execute(&GroupLayers {
        layers: text_ids,
        name: "Title".into(),
    })
    .map_err(err)?;
    // Folded away, as a finished title usually is, so the whole stack fits
    // the Layers panel.
    let title = top_id(&ed)?;
    ed.execute(&SetCollapsed {
        layer: title,
        collapsed: true,
    })
    .map_err(err)?;

    Ok(Editor::new(ed.doc().clone()))
}

/// The layer to select when the demo opens: the curves layer, so the
/// Properties panel starts on something to play with.
pub(crate) fn initial_layer(doc: &Document) -> Option<LayerId> {
    doc.layers()
        .iter()
        .find(|l| l.name == "Lift shadows")
        .map(|l| l.id)
}

/// Id of the layer on top of the stack.
fn top_id(ed: &Editor) -> Result<LayerId, String> {
    ed.doc()
        .layers()
        .last()
        .map(|l| l.id)
        .ok_or_else(|| "demo: empty layer stack".to_string())
}

/// Add an adjustment layer on top and give it a descriptive name.
fn add_adjustment(ed: &mut Editor, name: &str, adj: Adjustment) -> Result<LayerId, String> {
    let err = |e: lumenply_core::EditError| e.to_string();
    ed.execute(&AddAdjustmentLayer::new(adj)).map_err(err)?;
    let id = top_id(ed)?;
    ed.execute(&RenameLayer {
        layer: id,
        name: name.into(),
    })
    .map_err(err)?;
    Ok(id)
}

/// Sky mask value for row `y` of an `h`-row image: fully on above 30 % of
/// the height, smoothly off by 52 % (just under the ridge line).
pub(crate) fn sky_coverage(y: u32, h: u32) -> f32 {
    let t = y as f32 / h.max(1) as f32;
    1.0 - smoothstep(0.30, 0.52, t)
}

fn sky_mask(w: u32, h: u32) -> Mask {
    let mut m = Mask::hide_all();
    for y in 0..h {
        let v = sky_coverage(y, h);
        if v <= 0.0 {
            break;
        }
        for x in 0..w {
            m.set_value(x as i32, y as i32, v);
        }
    }
    m
}

/// Vignette darkness (alpha of a black pixel) at canvas position: clear
/// across the middle, deepening smoothly into the corners.
pub(crate) fn vignette_alpha(x: u32, y: u32, w: u32, h: u32) -> f32 {
    let nx = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
    let ny = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
    let r = ((nx * nx + ny * ny) / 2.0).sqrt(); // 0 centre, 1 corners
    0.85 * smoothstep(0.42, 1.0, r)
}

fn vignette(w: u32, h: u32) -> Raster {
    let mut r = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let a = vignette_alpha(x, y, w, h);
            r.set(x, y, Rgba::from_straight(0.0, 0.0, 0.0, a));
        }
    }
    r
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_photo_decodes_at_its_documented_size() {
        let r = photo_raster().unwrap();
        assert_eq!((r.width, r.height), (1800, 1205));
        // Opaque everywhere, and not a blank frame: the sky near the top
        // centre is brighter than the valley floor near the bottom.
        assert!(r.pixels.iter().all(|p| p.a == 1.0));
        let sky = r.get(900, 300);
        let valley = r.get(900, 1100);
        assert!(sky.r + sky.g + sky.b > 4.0 * (valley.r + valley.g + valley.b));
    }

    #[test]
    fn demo_builds_the_expected_stack() {
        let ed = build().unwrap();
        let doc = ed.doc();
        assert_eq!((doc.width, doc.height), (1800, 1205));
        let names: Vec<&str> = doc.layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Background",
                "Lift shadows",
                "Sky colour",
                "Warm grade",
                "Vignette",
                "Title"
            ]
        );
        let title = doc.layers().last().unwrap();
        let kids: Vec<&str> = title
            .children()
            .unwrap()
            .iter()
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(kids, ["MOUNT COOK NATIONAL PARK", "AORAKI"]);
        assert!(title.children().unwrap().iter().all(|l| l.text_layer().is_some()));
        // Only the sky adjustment carries a mask; the vignette sits at 60 %.
        let masked: Vec<&str> = doc
            .layers()
            .iter()
            .filter(|l| l.mask.is_some())
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(masked, ["Sky colour"]);
        assert_eq!(doc.layers()[4].opacity, 0.6);
        assert!(title.collapsed, "the title group opens folded");
        let first = initial_layer(doc).and_then(|id| doc.layer(id)).unwrap();
        assert!(matches!(
            first.content,
            LayerContent::Adjustment(Adjustment::Curves { .. })
        ));
        // Opens like a saved file: nothing to undo, nothing selected.
        assert_eq!(ed.history().len(), 0);
        assert!(doc.selection.is_none());
    }

    #[test]
    fn sky_mask_fades_out_above_the_ridge() {
        assert_eq!(sky_coverage(0, 1000), 1.0);
        assert_eq!(sky_coverage(300, 1000), 1.0);
        assert!((sky_coverage(410, 1000) - 0.5).abs() < 1e-6);
        assert!(sky_coverage(520, 1000) < 1e-6);
        assert_eq!(sky_coverage(999, 1000), 0.0);
    }

    #[test]
    fn vignette_is_clear_in_the_middle_and_dark_in_the_corners() {
        assert_eq!(vignette_alpha(500, 300, 1000, 600), 0.0);
        assert!((vignette_alpha(0, 0, 1000, 600) - 0.85).abs() < 0.01);
        let edge = vignette_alpha(0, 300, 1000, 600);
        assert!(edge > 0.2 && edge < 0.6, "edge midpoints are half dark: {edge}");
    }
}
