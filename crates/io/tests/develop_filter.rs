//! The Camera Raw Filter (`Filter::Develop`) in files: `.lumen` keeps it
//! editable (smart filter and live filter layer); PSD and OpenRaster bake
//! the smart filter into the pixels with the usual warning (ADR 0011).

use std::path::PathBuf;

use lumenply_doc::{Develop, Document, Filter, LayerContent, SmartFilter};
use lumenply_tiles::{Raster, Rgba, TileStore};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lumenply-crf-{}-{name}", std::process::id()))
}

fn plus_one_stop() -> Filter {
    Filter::Develop {
        settings: Develop {
            exposure: 1.0,
            clarity: 25.0,
            vignette_midpoint: 70.0,
            ..Develop::NEUTRAL
        },
        frame: [0, 0, 64, 32],
    }
}

/// 64×32 grey (0.2) layer under a +1 EV Camera Raw smart filter (clarity
/// on a flat layer changes nothing), with a live Camera Raw layer above.
fn developed() -> Document {
    let mut doc = Document::new(64, 32);
    let id = doc.add_pixel_layer("Photo");
    let r = Raster::filled(64, 32, Rgba::new(0.2, 0.2, 0.2, 1.0));
    let l = doc.layer_mut(id).unwrap();
    *l.pixels_mut().unwrap() = TileStore::from_raster(&r, 0, 0);
    l.smart_filters.filters.push(SmartFilter::new(plus_one_stop()));
    doc.add_filter(Filter::Develop {
        settings: Develop {
            vignette: -40.0,
            ..Develop::NEUTRAL
        },
        frame: [0, 0, 64, 32],
    });
    lumenply_render::fill::refresh_stale(&mut doc);
    doc
}

#[test]
fn the_filter_serialises_as_tagged_json() {
    let s = serde_json::to_string(&plus_one_stop()).unwrap();
    assert!(s.starts_with(r#"{"type":"develop","settings":{"#), "{s}");
    assert!(
        s.contains(r#""exposure":1.0"#) && s.ends_with(r#""frame":[0,0,64,32]}"#),
        "{s}"
    );
    let back: Filter = serde_json::from_str(&s).unwrap();
    assert_eq!(back, plus_one_stop());
}

#[test]
fn lumen_projects_keep_camera_raw_filters_editable() {
    let doc = developed();
    let path = temp("keep.lumen");
    lumenply_io::project::save(&path, &doc).unwrap();
    let mut back = lumenply_io::project::load(&path).unwrap();
    std::fs::remove_file(&path).ok();
    lumenply_render::fill::refresh_stale(&mut back);
    let l = &back.layers()[0];
    assert_eq!(l.smart_filters.filters, vec![SmartFilter::new(plus_one_stop())]);
    let p = l.raster_store().unwrap().get_pixel(32, 16);
    assert!((p.r - 0.4).abs() < 1e-3, "{p:?}");
    assert!(matches!(
        &back.layers()[1].content,
        LayerContent::Filter(Filter::Develop { settings, frame: [0, 0, 64, 32] }) if settings.vignette == -40.0
    ));
}

#[test]
fn psd_and_ora_export_bake_the_smart_filter_with_a_warning() {
    let doc = developed();
    let path = temp("bake.psd");
    let rep = lumenply_io::psd::save(&path, &doc).unwrap();
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.contains("smart filters were baked")),
        "{:?}",
        rep.warnings
    );
    assert!(
        rep.warnings.iter().any(|w| w.contains("Camera Raw Filter")),
        "the live layer is reported: {:?}",
        rep.warnings
    );
    let back = lumenply_io::psd::load(&path).unwrap().value;
    std::fs::remove_file(&path).ok();
    let l = &back.layers()[0];
    assert!(l.smart_filters.is_empty());
    // 0.4 linear survives 8-bit sRGB storage within half a level.
    let p = l.pixels().unwrap().get_pixel(32, 16);
    assert!((p.r - 0.4).abs() < 0.004, "{p:?}");

    let path = temp("bake.ora");
    let rep = lumenply_io::ora::save(&path, &doc).unwrap();
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.contains("smart filters were baked")),
        "{:?}",
        rep.warnings
    );
    let back = lumenply_io::ora::load(&path).unwrap().value;
    std::fs::remove_file(&path).ok();
    let p = back.layers()[0].pixels().unwrap().get_pixel(32, 16);
    assert!((p.r - 0.4).abs() < 0.004, "{p:?}");
}
