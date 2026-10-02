//! Shape layers through the file formats: `.lumen` keeps the vector
//! outline, transform, fill and stroke exactly and re-renders on load;
//! PSD writes Photoshop shape layers (fill + `vstk` + `vmsk`) and reads
//! them back as shapes; OpenRaster bakes them to pixels with a warning.

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

/// Three solid shapes, one per stroke alignment, through PSD: each comes
/// back with its alignment and width (and, with LUMENPLY_KEEP_PSD set,
/// stays on disk for the psd-tools render comparison).
#[test]
fn psd_keeps_each_stroke_alignment() {
    let mut doc = Document::new(240, 100);
    let kinds = [
        (
            ShapeGeometry::Ellipse {
                rect: [10.0, 15.0, 60.0, 70.0],
            },
            StrokeAlign::Inside,
        ),
        (
            ShapeGeometry::Rectangle {
                rect: [95.0, 20.0, 50.0, 60.0],
                radius: 0.0,
            },
            StrokeAlign::Outside,
        ),
        (
            ShapeGeometry::Polygon {
                rect: [165.0, 15.0, 65.0, 70.0],
                sides: 5,
            },
            StrokeAlign::Center,
        ),
    ];
    for (geometry, align) in kinds {
        let id = doc.alloc_id();
        doc.add_layer(Layer::shape(
            id,
            ShapeLayer::new(
                geometry,
                Some(Fill::Solid {
                    color: [0.1, 0.5, 0.9],
                }),
                Some(ShapeStroke {
                    color: [0.9, 0.2, 0.1],
                    width: 6.0,
                    align,
                    dash: None,
                }),
            ),
        ));
    }
    lumenply_render::fill::refresh_stale(&mut doc);
    let path = temp("strokes.psd");
    lumenply_io::psd::save(&path, &doc).unwrap();
    if let Ok(dir) = std::env::var("LUMENPLY_KEEP_PSD") {
        let _ = std::fs::copy(&path, PathBuf::from(dir).join("strokes.psd"));
    }
    let back = lumenply_io::psd::load(&path).unwrap().value;
    let _ = std::fs::remove_file(&path);
    let read: Vec<_> = shapes(&back)
        .iter()
        .map(|s| s.stroke.map(|st| (st.align, st.width)))
        .collect();
    assert_eq!(
        read,
        vec![
            Some((StrokeAlign::Inside, 6.0)),
            Some((StrokeAlign::Outside, 6.0)),
            Some((StrokeAlign::Center, 6.0))
        ]
    );
}

#[test]
fn psd_writes_photoshop_shape_layers_and_reads_them_back() {
    let doc = shapes_doc();
    let path = temp("shapes.psd");
    let report = lumenply_io::psd::save(&path, &doc).unwrap();
    // Kept for the psd-tools cross-check when asked to.
    if let Ok(dir) = std::env::var("LUMENPLY_KEEP_PSD") {
        let _ = std::fs::copy(&path, PathBuf::from(dir).join("shapes.psd"));
    }
    let back = lumenply_io::psd::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        report.warnings.iter().all(|w| !w.contains("shape")),
        "shapes export without warnings: {:?}",
        report.warnings
    );
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let (a, b) = (shapes(&doc), shapes(&back.value));
    assert_eq!(b.len(), 2, "both come back as shape layers");
    assert_eq!(back.value.layers()[0].name, "Rounded Rectangle");
    assert_eq!(back.value.layers()[0].blend, BlendMode::Multiply);
    for (orig, read) in a.iter().zip(&b) {
        // The outline comes back as a path in canvas coordinates, within
        // 8.24 fixed point of the original.
        let (p, q) = (orig.outline(), read.outline());
        assert_eq!(p.subpaths.len(), q.subpaths.len());
        for (sa, sb) in p.subpaths.iter().zip(&q.subpaths) {
            assert_eq!(sa.closed, sb.closed);
            assert_eq!(sa.nodes.len(), sb.nodes.len());
            for (na, nb) in sa.nodes.iter().zip(&sb.nodes) {
                for (u, v) in [
                    (na.point, nb.point),
                    (na.handle_in, nb.handle_in),
                    (na.handle_out, nb.handle_out),
                ] {
                    assert!(
                        (u.0 - v.0).abs() < 1e-3 && (u.1 - v.1).abs() < 1e-3,
                        "{u:?} {v:?}"
                    );
                }
            }
        }
        assert_eq!(
            orig.stroke.map(|s| (s.align, s.dash, s.width)),
            read.stroke.map(|s| (s.align, s.dash, s.width))
        );
        assert_eq!(orig.fill.is_some(), read.fill.is_some());
    }
    match &b[1].fill {
        Some(Fill::Solid { color }) => {
            assert!(
                (color[0] - 1.0).abs() < 1e-3 && (color[1] - 0.8).abs() < 5e-3,
                "{color:?}"
            )
        }
        f => panic!("star fill: {f:?}"),
    }
    assert!(matches!(
        b[0].fill,
        Some(Fill::Gradient {
            style: GradientStyle::Radial,
            reverse: true,
            ..
        })
    ));
    // Re-rendered from the vectors, the picture matches the original.
    let canvas = doc.canvas();
    let (ca, cb) = (
        lumenply_render::composite_layers(doc.layers(), canvas, canvas),
        lumenply_render::composite_layers(back.value.layers(), canvas, canvas),
    );
    for (x, y) in [(120, 80), (230, 140), (60, 40), (5, 5), (229, 95)] {
        let (p, q) = (ca.get_pixel(x, y), cb.get_pixel(x, y));
        assert!(
            (p.r - q.r).abs() < 0.01 && (p.g - q.g).abs() < 0.01 && (p.a - q.a).abs() < 0.01,
            "({x}, {y}) {p:?} {q:?}"
        );
    }
}
