//! The newer adjustments and fill layers through the file formats:
//! `.lumen` keeps every parameter exactly; PSD carries fills as `SoCo` /
//! `GdFl` blocks beside their rendered pixels; OpenRaster bakes them.

use std::path::PathBuf;

use lumenply_doc::{
    Adjustment, BlendMode, Document, Fill, Gradient, GradientStop, GradientStyle, Layer, LayerContent, Mask,
};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-adjfill-{}-{name}", std::process::id()))
}

/// A document with a solid fill (masked, multiplied) over a gradient fill
/// (radial, with a fading stop), caches rendered.
fn fills_doc() -> Document {
    let mut doc = Document::new(300, 200);
    let id = doc.alloc_id();
    let mut grad = Layer::fill(
        id,
        Fill::Gradient {
            gradient: Gradient {
                stops: vec![
                    GradientStop::srgb8(0.0, [255, 128, 0]),
                    GradientStop {
                        alpha: 0.25,
                        ..GradientStop::srgb8(1.0, [0, 64, 255])
                    },
                ],
            },
            style: GradientStyle::Radial,
            angle: 30.0,
            scale: 0.8,
            reverse: true,
            offset: [0.1, -0.2],
        },
    );
    grad.name = "Sunset".into();
    doc.add_layer(grad);
    let id = doc.alloc_id();
    let mut solid = Layer::fill(
        id,
        Fill::Solid {
            color: [0.2, 0.4, 0.6],
        },
    );
    solid.blend = BlendMode::Multiply;
    solid.opacity = 0.75;
    let mut mask = Mask::hide_all();
    for y in 0..100 {
        for x in 0..150 {
            mask.set_value(x, y, 1.0);
        }
    }
    solid.mask = Some(mask);
    doc.add_layer(solid);
    lumenply_render::fill::refresh_stale(&mut doc);
    doc
}

fn fills(doc: &Document) -> Vec<Fill> {
    doc.layers()
        .iter()
        .filter_map(|l| l.fill_layer().map(|f| f.fill.clone()))
        .collect()
}

#[test]
fn lumen_keeps_fill_layers_and_re_renders_them() {
    let doc = fills_doc();
    let path = temp("fills.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let back = lumenply_io::project::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(fills(&back), fills(&doc));
    assert_eq!(back.layers()[1].blend, BlendMode::Multiply);
    assert!(back.layers()[1].mask.is_some());
    // The caches were rebuilt on load: the composite matches.
    let (a, b) = (
        lumenply_render::composite(&doc),
        lumenply_render::composite(&back),
    );
    for (x, y) in [(10, 10), (150, 100), (299, 199), (200, 20)] {
        assert_eq!(a.get_pixel(x, y), b.get_pixel(x, y), "({x}, {y})");
    }
}

/// Also leaves `<tmp>/lumenply-psd-extra/fills.psd` for psd-tools.
#[test]
fn psd_carries_fill_settings_beside_their_pixels() {
    let doc = fills_doc();
    let dir = std::env::temp_dir().join("lumenply-psd-extra");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fills.psd");
    let report = lumenply_io::psd::save(&path, &doc).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let back = lumenply_io::psd::load(&path).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let got = fills(&back.value);
    assert_eq!(got.len(), 2);
    match &got[1] {
        Fill::Solid { color } => {
            // 8-bit sRGB doubles: within half a step.
            for (c, e) in color.iter().zip([0.2, 0.4, 0.6]) {
                assert!((c - e).abs() < 3e-3, "{color:?}");
            }
        }
        other => panic!("{other:?}"),
    }
    let Fill::Gradient {
        gradient,
        style,
        angle,
        scale,
        reverse,
        offset,
    } = &got[0]
    else {
        panic!("{:?}", got[0]);
    };
    assert_eq!(*style, GradientStyle::Radial);
    assert!((angle - 30.0).abs() < 1e-4 && (scale - 0.8).abs() < 1e-4 && *reverse);
    assert!((offset[0] - 0.1).abs() < 1e-4 && (offset[1] + 0.2).abs() < 1e-4);
    assert_eq!(gradient.stops.len(), 2);
    assert!(
        (gradient.stops[1].alpha - 0.25).abs() < 1e-4,
        "{:?}",
        gradient.stops
    );
    assert_eq!(back.value.layers()[0].name, "Sunset");
    let l = &back.value.layers()[1];
    assert_eq!(l.blend, BlendMode::Multiply);
    assert!((l.opacity - 0.75).abs() < 3e-3 && l.mask.is_some());
    // The rendered pixels came back too (8-bit): same composite, roughly.
    let (a, b) = (
        lumenply_render::composite(&doc),
        lumenply_render::composite(&back.value),
    );
    for (x, y) in [(10, 10), (299, 199), (200, 20)] {
        let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
        assert!(
            (p.r - q.r).abs() < 0.01 && (p.b - q.b).abs() < 0.01,
            "({x}, {y}) {p:?} {q:?}"
        );
    }
}

#[test]
fn ora_bakes_fill_layers_with_a_warning() {
    let doc = fills_doc();
    let path = temp("fills.ora");
    let report = lumenply_io::ora::save(&path, &doc).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|w| w.contains("fill layer"))
            .count(),
        2,
        "{:?}",
        report.warnings
    );
}

#[test]
fn lumen_keeps_every_new_adjustment_exactly() {
    let mut doc = Document::new(16, 16);
    doc.add_pixel_layer("Background");
    let mut colors = [[0.0f32; 4]; 9];
    colors[2] = [0.1, -0.2, 0.3, -0.4];
    let adjs = vec![
        Adjustment::GradientMap {
            gradient: Gradient {
                stops: vec![
                    GradientStop::srgb8(0.0, [10, 20, 30]),
                    GradientStop::srgb8(0.4, [200, 100, 50]),
                    GradientStop::srgb8(1.0, [250, 240, 230]),
                ],
            },
            reverse: true,
        },
        Adjustment::ChannelMixer {
            red: [1.1, -0.1, 0.0, 0.02],
            green: [0.0, 0.9, 0.1, 0.0],
            blue: [0.05, 0.0, 0.95, -0.03],
            monochrome: false,
            gray: [0.3, 0.6, 0.1, 0.0],
        },
        Adjustment::PhotoFilter {
            color: [0.1, 0.2, 0.9],
            density: 0.37,
            preserve_luminosity: false,
        },
        Adjustment::SelectiveColor {
            colors,
            absolute: true,
        },
    ];
    for a in &adjs {
        doc.add_adjustment(a.clone());
    }
    let path = temp("adjust.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let back = lumenply_io::project::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let got: Vec<Adjustment> = back
        .layers()
        .iter()
        .filter_map(|l| match &l.content {
            LayerContent::Adjustment(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(got, adjs);
}
