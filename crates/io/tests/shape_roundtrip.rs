//! Shape layers through the file formats: `.lumen` keeps the vector
//! outline, transform, fill and stroke exactly and re-renders on load;
//! OpenRaster bakes them to pixels with a warning.

use std::path::PathBuf;

use lumenply_doc::shape::{CustomShape, ShapeGeometry, ShapeLayer, ShapeStroke, StrokeAlign};
use lumenply_doc::{BlendMode, Document, Fill, Gradient, GradientStyle, Layer};
use lumenply_tiles::Affine;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-shape-{}-{name}", std::process::id()))
}

/// A rotated rounded rectangle with a gradient fill and a dashed stroke,
/// and a star with a solid fill, caches rendered.
fn shapes_doc() -> Document {
    let mut doc = Document::new(300, 200);
    let id = doc.alloc_id();
    let mut rr = ShapeLayer::new(
        ShapeGeometry::Rectangle {
            rect: [40.0, 30.0, 160.0, 100.0],
            radius: 18.0,
        },
        Some(Fill::Gradient {
            gradient: Gradient::default(),
            style: GradientStyle::Radial,
            angle: 30.0,
            scale: 0.8,
            reverse: true,
            offset: [0.1, -0.2],
        }),
        Some(ShapeStroke {
            color: [0.9, 0.1, 0.2],
            width: 5.0,
            align: StrokeAlign::Center,
            dash: Some([3.0, 1.5]),
        }),
    );
    rr.transform_by(&Affine::around(120.0, 80.0, 1.2, 0.9, 0.3));
    let mut l = Layer::shape(id, rr);
    l.blend = BlendMode::Multiply;
    l.opacity = 0.8;
    doc.add_layer(l);
    let id = doc.alloc_id();
    doc.add_layer(Layer::shape(
        id,
        ShapeLayer::new(
            ShapeGeometry::Custom {
                rect: [180.0, 90.0, 100.0, 100.0],
                shape: CustomShape::Star,
            },
            Some(Fill::Solid {
                color: [1.0, 0.8, 0.0],
            }),
            None,
        ),
    ));
    lumenply_render::fill::refresh_stale(&mut doc);
    doc
}

fn shapes(doc: &Document) -> Vec<ShapeLayer> {
    doc.layers()
        .iter()
        .filter_map(|l| l.shape_layer().cloned())
        .collect()
}

#[test]
fn lumen_keeps_shape_layers_and_re_renders_them() {
    let doc = shapes_doc();
    let path = temp("shapes.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let back = lumenply_io::project::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(shapes(&back), shapes(&doc));
    assert_eq!(back.layers()[0].name, "Rounded Rectangle");
    assert_eq!(back.layers()[1].name, "Star");
    assert_eq!(back.layers()[0].blend, BlendMode::Multiply);
    // The caches were rebuilt on load: the composites match exactly.
    let canvas = doc.canvas();
    let (a, b) = (
        lumenply_render::composite_layers(doc.layers(), canvas, canvas),
        lumenply_render::composite_layers(back.layers(), canvas, canvas),
    );
    for (x, y) in [(120, 80), (230, 140), (60, 40), (5, 5), (229, 95)] {
        assert_eq!(a.get_pixel(x, y), b.get_pixel(x, y), "({x}, {y})");
    }
    // The star's middle is painted yellow.
    let p = b.get_pixel(230, 140);
    assert!(p.a > 0.99 && p.b < 0.01 && p.r > 0.99, "{p:?}");
}

#[test]
fn ora_bakes_shape_layers_with_a_warning() {
    let doc = shapes_doc();
    let path = temp("shapes.ora");
    let report = lumenply_io::ora::save(&path, &doc).unwrap();
    let back = lumenply_io::ora::load(&path).unwrap().value;
    let _ = std::fs::remove_file(&path);
    let warned: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.contains("shape layer"))
        .collect();
    assert_eq!(warned.len(), 2, "{:?}", report.warnings);
    assert!(warned[0].contains("'Star' (Star)"), "top-down: {warned:?}");
    assert_eq!(back.layers().len(), 2);
    assert!(back.layers().iter().all(|l| l.pixels().is_some()));
    assert!(back.layers()[1].pixels().unwrap().get_pixel(230, 140).a > 0.99);
}
